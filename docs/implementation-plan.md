# Implementation plan

## Working agreement

Stage 1 is deployed; see [verification](stage-1-verification.md). The existing
public repository is `Math1987/aithos-calendar`; the earlier planning commits are
preserved. Later stages remain behind their acceptance gates.

Each stage ends with a concrete manual acceptance check and a report of the
deployed commit, changes, and remaining limitations. Wait for acceptance before
starting the next stage. Later stages are a roadmap, not current implementation
scope. Do not provision their resources in advance.

## Stages

| Stage | Deliverable | Manual acceptance | Resources added |
| --- | --- | --- | --- |
| **1. Production foundation** | GitHub OIDC CI/CD; health API and blank website on their final domains | Push to `main`; inspect successful deployment; curl exact health JSON; open blank HTTPS site; repeat deployment safely | S3 state and website buckets, IAM/OIDC, Lambda, HTTP API, CloudWatch logs, CloudFront, ACM and scoped Route 53 records |
| 2. Public-page reading and share links | Replaceable HTTP reader; home, host booking page and tutorial; no booking writes | Paste a valid host URL and open its share link on another device; compare metadata with Google; try an invalid URL | Add persistence for links only when the design requires it, likely one DynamoDB table; no booking secret |
| 3. Availability matching | Read visitor page and select a host-duration interval; identity/ownership policy decided before enabling bookings | Use two controlled calendars with different appointment durations, an existing event, and no-overlap cases; verify the entire selected interval | Usually reuse existing resources; add another reader only if public pages cannot meet the requirement |
| 4. Real booking through Anakin | Provider adapter, durable operation, spinner/status, confirmed success/error | Book once on controlled calendars; verify invitee identity and both calendars; refresh/resubmit; exercise failure and uncertain-outcome reconciliation | Anakin account and Secrets Manager secret; durable operation storage and the smallest suitable execution mechanism |
| 5. A2A collaboration | Two discoverable logical agents on the shared service call the same scheduling application | Discover both Agent Cards, negotiate a time, create one meeting and verify isolation between agents | SDK/transport and persisted agent configuration; reuse infrastructure unless measurements require changes |

Stage 2 can display only the host page until stage 3 is ready. Stage 3 matching
verification may use a temporary developer-facing command; do not add a permanent
confirmation screen to the agreed final product flow. In the final flow, pressing
"Find a time and book" proceeds directly to the spinner and booking outcome.

At stage 3, decide whether conservative public-page coverage is acceptable; it
does not prove calendar availability outside exposed booking intervals. Resolve
the booking identity, ownership/authorization, and required email handling before
stage 4. A common service mailbox remains an option to evaluate, not an accepted
substitute for the visitor's identity.

At stage 4, do not assume Anakin has an idempotency guarantee. Validate its actual
submission and job-result contract before allowing retries. Never submit another
booking just because a previous request timed out.

Reevaluate ZIP packaging when adding Python dependencies in stages 2–5,
especially the A2A SDK. Use deterministic Linux-compatible dependency packaging
before considering containers. No LLM is required by these stages.

## Next action

Review the [stage-1 verification](stage-1-verification.md) and manually open both
production URLs. Stage 2 begins only after owner acceptance and explicit instruction.
