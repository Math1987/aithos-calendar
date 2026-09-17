# Gate 6: browser booking

## State — 17 September 2026

Implementation is on `codex/browser-booking`, not deployed. Gate 5 was accepted.
The browser and durable operation flow are implemented and tested with fake
provider outcomes. A real Anakin booking has **not** been submitted yet.

**Deployment prerequisite:** capture one explicitly approved real `gap_book_slot`
result, implement the exact confirmation validator in `booking::confirmed`, and
add a redacted response fixture. That function currently returns false on purpose.
Do not merge/deploy this draft as a finished booking service. Anakin's public
catalog provides the input fields but no booking output schema. A completed job
alone is insufficient evidence that the requested appointment was reserved.

## Flow

1. The existing A2A exchange finds a complete offered host slot covered by the
   visitor's advertised intervals. This remains deterministic and read-only.
2. The browser persists a random operation UUID and the proposed host/visitor/slot
   locally before POST `/bookings`. It stores no attendee names or email.
3. The server rereads both Google pages and validates the exact proposed slot.
   The attendee comes from the visitor page; missing fields return 422 and the
   browser asks only for those fields.
4. One DynamoDB transaction creates the operation and locks both agents plus the
   pair/slot. Only the winner submits an Anakin job. The browser polls
   GET `/bookings/{id}`. Each request ends normally; no background Lambda task,
   queue, Step Functions state machine or LLM is needed.
5. A persisted polling lease limits duplicate polls from multiple tabs.
6. Only a verified provider confirmation may yield `booked` / `reserved:true`.
   An ambiguous submission, failed asynchronous job, or unsupported completion
   yields `unknown`. No automatic retry is made.

Refresh resumes the saved operation with GET, never a new POST. The API also
makes a repeated POST with the same UUID and input idempotent. Reusing the UUID
with different input returns 409. A new UUID cannot bypass an active agent lock.
The API is public as agreed: a page URL does not authenticate its owner.
Operation UUIDs act as unguessable status references; there is no listing route.

## Persistence and permissions

`calendar-production-bookings` uses conditional DynamoDB transactions, encryption,
point-in-time recovery and deletion protection. The public status excludes the
provider job ID, contact details, raw response, and confirmation URLs. The saved
record keeps digests of the request and attendee email instead of plaintext.

Known rejection before job submission releases all guards. Verified success
releases the agent locks but retains the pair/slot guard until after the slot
ends. Unknown outcomes retain the agent locks and operation for reconciliation.
Only resolved operation records expire (30 days after the appointment); pair/slot
guards expire one day after its end. DynamoDB TTL cleanup is asynchronous.

These are application-level duplicate protections, not a claim of transactional
exactly-once delivery across AWS and Anakin. A provider may write before a timeout.

Bootstrap was applied: scoped booking permissions and metadata for Secrets Manager
secret `calendar/production/anakin`. Its API key value was loaded separately and
verified without output; it never enters Terraform state. Warm Lambda instances
cache the client. After key rotation, recycle Lambda environments to reload it.
The production table/routes have not been applied yet.

## Manual acceptance after deployment

- Use two controlled pages and check the contact associated with the visitor page.
- Open the host share link, paste the visitor link, and choose **Find and book a time**.
  This authorizes a real booking and Google confirmation, using Anakin credits.
- Verify the exact date/time/duration, host appointment, visitor email invitation
  and whether it blocks the visitor's calendar. Receiving an invitation is not
  necessarily the same as accepting/blocking it under every Google account setting.
- Refresh while pending and after completion: the same operation must resume.
- Inspect `calendar::booking` logs; never log attendee data, API keys, or raw results.
- An unknown result requires checking Anakin and Google before another attempt.
  Operator reconciliation must match the stored job, schedule and exact slot before
  changing state or conditionally releasing guards owned by this operation.
  Never clear all locks or resubmit merely because an HTTP request timed out.

## Validation

`cargo test --locked`, both Terraform roots' `validate`, and
`python3 scripts/check-web.py` cover the code/configuration.
`python3 scripts/preview-web.py --booking-result missing` supplies a fully isolated
browser flow (no AWS/Google/Anakin calls): missing contact → pending → booked →
refresh. Other preview outcomes are `unknown` and `failed`.

A controlled operator test can pin an explicitly approved time with
`BOOKING_EXPECTED_START=2026-09-18T07:00:00Z` when running the existing booking
example. The journal must be new, private, and polled rather than resubmitted.
The pin causes an abort if the earliest common slot changes.
