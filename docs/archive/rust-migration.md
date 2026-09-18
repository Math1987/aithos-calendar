# Rust health migration

Status: deployed and verified on 2026-09-16, including repeatability.

## Scope and choices

Replace only the deployed Python health handler. Keep the same Lambda resource,
API Gateway route, domains, execution role, architecture, memory and timeout.
No additional AWS resources, application routes, A2A SDK or LLM are introduced.
The blank website is unchanged. Existing Python operator scripts remain tooling;
the deployed application is Rust.

- Rust 1.95.0, edition 2024; exact direct dependencies and tracked `Cargo.lock`.
- `lambda_http` 1.3.1 with only the HTTP API event feature, plus Tokio 1.53.1.
- A single-thread Tokio runtime is sufficient for this handler.
- `provided.al2023` runs the Linux executable named `bootstrap` at the ZIP root.
- Build `x86_64-unknown-linux-musl` on Ubuntu 24.04 with `musl-gcc`; static linking
  avoids building against the runner's newer glibc. Preserve x86_64 to limit scope.
- Terraform's existing archive resource sets executable mode 0755 and tracks the
  ZIP hash. No Docker, Cargo Lambda, Axum or extra artifact storage is needed.

AWS documents the [Rust ZIP contract](https://docs.aws.amazon.com/lambda/latest/dg/rust-package.html)
and [HTTP event adapter](https://docs.aws.amazon.com/lambda/latest/dg/rust-http-events.html).
Cargo Lambda is an optional convenience; plain Cargo suffices for this Linux CI.

## Local checks

```sh
cargo fmt --check
cargo build --locked
terraform -chdir=infra/production validate
```

Native builds are for local checks only. Do not copy a macOS executable into
`.build/bootstrap`. Production packaging requires the Linux musl build performed
by the workflow. `target/`, `.build/`, ZIP files and local credentials are ignored.

## Acceptance

1. Formatting and native compilation pass; exercise the executable against a
   local Lambda Runtime API with an API Gateway v2 event.
2. Terraform validates; review the production apply for an in-place Lambda update.
3. CI confirms runtime `provided.al2023`, active/successful function configuration
   and the exact JSON response from the public health URL.
4. Check website and unknown-route behavior remain unchanged.
5. Repeat deployment and verify Terraform reports no resource changes.
6. Inspect Lambda invocation logs when a valid operator AWS session is available.

## Verification record

- Application commit: `6f2a8ccb612bf43027eb508206b37cbad16fba26`.
- [Initial production deployment](https://github.com/Math1987/aithos-calendar/actions/runs/35058574393/attempts/1): successful.
- Native compile, formatting, Terraform validation and local Runtime API smoke
  check passed. The smoke check sent an API Gateway v2 event through the actual
  executable and asserted status, JSON body, content type and no-store headers.
- CI produced an x86-64 static PIE Linux executable. Terraform changed only
  `aws_lambda_function.health` in place: 0 added, 1 changed, 0 destroyed.
- AWS reports `provided.al2023`, `Active`, `Successful`. Deployed ZIP SHA-256:
  `HuVEgGM9fu0PT+Blo1C2cen8NesZFyl8Pi6Oh7cvju8=`.
- Public HTTPS health returns exact expected JSON and headers; unknown route
  returns 404; website returns the original empty-body HTML with HTTP 200.
- CloudWatch confirms a successful invocation of the provided AL2023 runtime.
  The first observed report used 19 MB; initialization was 34.81 ms and handler
  duration 1.14 ms. These are one observation, not a performance guarantee.
- No A2A or catalog endpoint has been implemented in this migration.
- [Repeat deployment](https://github.com/Math1987/aithos-calendar/actions/runs/35058574393/attempts/2): successful; 0 added, 0 changed, 0 destroyed. A fresh Linux build produced the same deployed ZIP hash and passed the public health check again.
