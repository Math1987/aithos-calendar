# Rust health migration

Status: implementation ready; production verification pending.

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

Pending deployment.
