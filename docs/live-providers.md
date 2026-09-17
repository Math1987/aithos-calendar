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
Responses must refer to the requested schedule. Title, time zone, duration, offered intervals, and optional public contact details
are retained. Contact details are excluded from serialized availability and
redacted in its debug representation; unrelated fields are ignored.
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

### Page-derived attendee

The visitor page supplies the attendee for the host page’s appointment. The
reader extracts the public display name and email associated with the schedule.
This is contact discovery, **not authenticated ownership** of the pasted page.
The decision to use it replaces the earlier fixed test attendee configuration.

A two-word, name-like display name is split into first and last name. This is a
heuristic: two-word organization names or reversed name order cannot reliably be
distinguished. Other formats (including mononyms and compound names) return the
specific missing first/last name fields rather than inventing a split. Email is
checked against a conservative address syntax, not verified for deliverability.
Missing or malformed identity never prevents reading availability.

`Attendee::from_page` accepts optional supplied details and returns either a usable
attendee or structured `missing_fields`. The terminal booking command prompts only
for those fields when attached to a terminal. Without an interactive terminal it
stops before submission with a list of required fields. Existing `BOOKING_TEST_*`
variables are ignored; there is no fallback to the host or a shared mailbox.
The future web flow will use the same result to request details when necessary.
Contact data is not added to agent cards, catalog entries, ordinary logs or A2A
availability serialization. The Anakin booking request necessarily includes it.

Before submission, the command re-reads both schedules and checks that the visitor
page’s contact has not changed. The final invitation/calendar effect and any email
verification still need a controlled real booking test.

### Local configuration and read-only preparation

Only the provider credential is needed in the ignored `.env` for submission:

```dotenv
ANAKIN_API_KEY=your_key
```

No provider key or fixed attendee configuration is needed for a read-only check:

```sh
cargo run --locked --example availability -- "$HOST_BOOKING_URL" "$GUEST_BOOKING_URL"
cargo run --locked --example booking -- prepare "$HOST_BOOKING_URL" "$GUEST_BOOKING_URL"
```

The first command reports identity readiness/missing fields without printing the
contact values. The second prepares and validates the booking request without
contacting Anakin, writing a journal, or creating an appointment.

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
   and update the existing UI to real metadata/results and conditional attendee
   fields. Public page ownership remains unverified.
6. Deploy and manually test busy slots, unequal durations, no overlap, time zones,
   provider failure, duplicate submits and confirmation in both calendars.

No LLM is needed for this deterministic provider integration.

## Identity extraction verification (2026-09-17)

- The full Rust suite passed: 28 tests, with the existing live network test ignored.
- Read-only `booking prepare` passed with the two test pages in both host/visitor
  directions, without prompting for attendee details. Both selected the offered
  30-minute slot starting at `2026-09-18T07:00:00Z`.
- Preparation printed no contact values and made no Anakin request. No appointment
  was booked. The production website remains on the mock flow until the remaining
  integration above is completed.
