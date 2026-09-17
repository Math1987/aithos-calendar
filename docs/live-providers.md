# Real availability and booking provider checks

Status: implementation in progress, September 17, 2026. Production still runs
the verified mock A2A/browser flow. These adapters are not yet wired into it.

## Availability: our Rust HTTP reader

`AvailabilityReader` is the replaceable boundary. `GoogleHttpReader` resolves no
user credentials and performs read-only calls against Google’s public booking
page backend. It gets public application configuration from the page, then reads
`GetAppointmentServiceDefinition` and `ListAvailableSlots`. This is an undocumented
protocol, isolated in one module; parsing changes fail closed.

Only approved Google HTTPS destinations are used. RPC requests do not redirect.
Bodies, timeouts, slots, duration and the search window (30 days) are bounded.
Responses must refer to the requested schedule. Only title, time zone, duration
and offered intervals are retained; owner email and unrelated fields are ignored.
No page or provider response body is logged.

Selection preserves an actual host start and complete host duration. Visitor
intervals may be merged only when overlapping or adjacent; a gap is never treated
as free. Different appointment durations are supported. Availability outside the
public pages’ coverage is unknown. Busy-calendar configuration remains Google’s
responsibility and must be tested manually with controlled events.

Read-only test (use your own two public booking pages):

```sh
cargo run --locked --example availability -- "$HOST_BOOKING_URL" "$GUEST_BOOKING_URL"
```

The real two-page check on September 17 read both pages successfully and found
September 18, 2026, 07:00–07:30 UTC (09:00–09:30 Europe/Paris). Both pages then had
a 30-minute appointment duration. This was a read-only result, not a reservation.

## Booking: Anakin Wire

Official sources checked September 17:

- [Google appointments action](https://anakin.io/catalog/google_appointments)
- [Submit a task](https://anakin.io/docs/api-reference/wire/execute-task)
- [Poll a job](https://anakin.io/docs/api-reference/wire/get-job)

`BookingProvider` separates submission and status lookup. `AnakinBooking` calls
`gap_book_slot` through `POST /v1/wire/task`, stores the job ID at the calling
application boundary, and reads `/v1/wire/jobs/{id}`. The provider catalog reports
10 credits per booking call, anonymous Google access, and required first name,
last name and email. The account API key was accepted by a protected read-only
endpoint. The public catalog alone is not an authentication test.

The live parameter description says `duration_minutes` is returned in the result
but is **not used for slot selection**. We therefore need a fresh host read and
a verified actual confirmation, not merely echoed request fields.

Do not treat job `completed` as proof of a booking. The transport currently
returns `CompletedUnverified(data)` until the action’s real output has been
validated. No success is exposed to the website by this adapter.

### Local configuration

Keep these values in the ignored `.env`; `scripts/with-env.py` supports them:

```dotenv
ANAKIN_API_KEY=your_key
BOOKING_TEST_EMAIL=your_test_inbox
# Optional overrides; otherwise Calendar / Guest:
# BOOKING_TEST_FIRST_NAME=Calendar
# BOOKING_TEST_LAST_NAME=Guest
```

The first real test uses this one fixed attendee, per the owner’s decision.
Names default to Calendar / Guest. Email has no default: provide one real inbox
you control. Google sends booking communications to that identity and may require
a verification code. A shared service attendee can be reused for controlled
tests, but it does not invite the actual visitor or ensure their calendar becomes
busy. Do not infer a visitor identity from a public booking URL.

### Controlled one-booking test

This command **creates a real booking and uses provider credits**. First inspect
the read-only result and ensure both pages are suitable for a controlled test.
It selects the earliest common host slot, re-reads both schedules, and submits
once. The journal must not already exist.

```sh
mkdir -p .build
python3 scripts/with-env.py cargo run --locked --example booking -- \
  submit .build/booking-test.jsonl "$HOST_BOOKING_URL" "$GUEST_BOOKING_URL"

python3 scripts/with-env.py cargo run --locked --example booking -- \
  poll .build/booking-test.jsonl
```

The private journal is created with mode 0600, locked, append-only and flushed
before submission and after saving the job ID. Polling never submits a booking.
A crash after submission but before saving the job ID is an **unknown outcome**:
reconcile it with Anakin and the calendar; do not create a new journal to retry.
The journal is an operator-test safeguard, not the future production task store.

After a terminal job, inspect the private result, check the email confirmation
and both calendars, and re-read availability. A failure may still need manual
reconciliation before another attempt. Google may require email verification.

## Remaining integration

1. Verify a real Anakin booking result and define strict confirmation parsing.
2. Wire real reads and host-slot selection into A2A. Existing signed mock cards
   need an explicit versioned update preserving agent identity and registry keys.
   Keep old fixture agents and real page agents distinct.
3. Store durable booking operations in AWS before the provider write; use atomic
   creation and request reuse. Persist the provider job ID, distinguish pending,
   confirmed, failed and unknown outcomes. Never retry an uncertain write.
4. Use bounded API calls and status polling, not a Lambda waiting for the whole
   provider job. The UI resumes the same operation after a reload.
5. Store the Anakin secret on AWS, make it accessible only to the booking runtime,
   and update the existing UI to real metadata/results. Keep test attendee use
   clearly identified until the final visitor identity flow is decided.
6. Deploy and manually test busy slots, unequal durations, no overlap, time zones,
   provider failure, duplicate submits and confirmation in both calendars.

No LLM is needed for this deterministic provider integration.
