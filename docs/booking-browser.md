# Gate 6: browser booking

## State — 17 September 2026

The browser proposes the first shared host appointment **from tomorrow** in the
host schedule's IANA time zone, within a 30-day horizon from now. This is a calendar
day rule, not a rolling 24-hour delay. It respects daylight-saving changes.
The visitor reviews the exact time and clicks **Book** to submit a real request.
Editing the visitor link clears the proposal. No LLM is involved.

**First-live-test limitation:** no real Anakin appointment has been submitted by
us yet. Its public catalog does not specify the booking confirmation schema.
A completed provider job therefore returns `confirmation_required`, not `booked`.
The UI says the request completed and asks the visitor to check Google's email
and calendar. The saved provider job ID allows read-only inspection afterward.
Once that actual response and appointment are checked, add a redacted fixture and
a validator for automatic confirmed success. Never submit a second job to learn
the first job's outcome. This is a manual validation pilot, not a completed gate 6.

## Flow

1. The existing A2A exchange finds a complete offered host slot covered by the
   visitor's advertised intervals. This remains deterministic and read-only.
2. After the visitor clicks **Book**, the browser persists a random operation UUID and the proposed host/visitor/slot
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
   Anakin completion currently yields `confirmation_required`. An ambiguous
   submission or failed asynchronous job yields `unknown`. Neither state is
   automatically retried and both retain the agent locks for reconciliation.

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
The production configuration adds the booking table, POST and GET routes, and a
28-second Lambda timeout within the existing 29-second API integration timeout.

## Manual acceptance after deployment

- Use two controlled pages and check the contact associated with the visitor page.
- Open the host share link, paste the visitor link, and choose **Find a time**. Verify that the proposal is
  tomorrow or later in the host time zone. Then click **Book**. Only this click
  authorizes a real booking and Google confirmation, using Anakin credits.
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
