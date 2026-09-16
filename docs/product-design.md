# Product design

Status: product direction agreed; booking identity and full conflict checking
remain open. Public agent onboarding and a browser mock preview are implemented; availability
reading and booking are later gates.

## Purpose

Let two people schedule an appointment using their public Google Calendar
appointment-schedule pages. The host shares a Calendar link; the visitor pastes
their own Google booking-page link; the service finds and reserves a suitable time.

Here, a "public calendar link" means a Google **appointment booking-page link**,
including `calendar.app.google` short links. It does not mean an ICS feed or a
publicly shared calendar containing events.

## Three pages

### 1. Home: create a shareable link

Route: `/`.

- One short explanation.
- One input: "Your Google booking-page link".
- One action: "Create link".
- On success, display a shareable Calendar URL and a copy action.
- On failure, show a short actionable message beside the input.

Share URL: `/book/{id}`. Public onboarding derives a stable agent ID from the
canonical Google booking-page URL. Repeated submissions return the same agent,
card and link; one person may have several pages and therefore several agents.
No user login is required. Creating a link does not reserve a meeting
or prove that the submitter owns the Google page.

### 2. Booking: let the visitor supply their availability

Route: `/book/{id}`.

- Show the host's appointment title, duration, and relevant description from the
  target Google page, for example "Talk about AI — 30 minutes".
- One input: "Your Google booking-page link".
- A tutorial link: "How do I create my Google booking-page link?".
- One explicit action: "Find a time and book".

Submitting this form authorizes one appointment for this request. Disable repeated
submission while processing and display a spinner with short loading text.

Terminal presentation:

- Success: "Booked for [weekday, date] at [time] ([timezone])." Show the timezone
  explicitly and convert the confirmed instant for the visitor.
- Error: explain the problem briefly and offer an appropriate next action.

Do not display success until the booking provider confirms a booking. If a network
failure leaves the booking outcome unknown, do not claim that no booking exists
or encourage a blind resubmission. Reconcile the original request first; keep the
same operation when the page reloads. The visual design remains spinner, success,
or error even if the backend has more detailed processing states.

No account creation, dashboard, date picker, chat, or settings screen is planned
for this initial product experience.

### 3. Tutorial

Route: `/help/google-booking-page`.

Explain only how to create a Google appointment schedule and copy its public
booking-page link. Mention that availability must check the relevant calendars.
Use current Google help as the reference when implementing the tutorial.

## Scheduling rules

1. The shared link identifies the **host / target** appointment schedule.
2. The host's page determines the appointment title, allowed starts, and duration.
3. Search the next 30 days, respecting the host's own scheduling constraints.
4. Choose the earliest host slot for which the visitor is available throughout
   the complete appointment interval.
5. The visitor's appointment duration does **not** have to match the host's.
6. Reserve once on the host's page. Do not create a second independent booking
   on the visitor's page.
7. Recheck availability immediately before booking; handle a competing booking
   without reporting false success or creating duplicates.

### Availability limitation to resolve later

A visitor's booking page exposes the appointments they offer, not a complete
free/busy view of their calendar. Absence of a bookable slot does not prove that
they are busy. Correctness also depends on the Google schedule checking all
relevant calendars.

A conservative candidate approach is to require the entire host interval to be
covered by the visitor's exposed available intervals. This can miss times when
the visitor is actually free. Validate that approach with different durations,
start grids, buffers, and existing events before accepting it as the product rule.
Never interpret an unknown interval as free. Exact conflict checking outside
those exposed intervals may require another availability source.

## Booking identity: deferred decision

Anakin's booking action requests first name, last name, and email. The email
identifies the guest and is used for booking communications/invitations. Google
can additionally require email verification, depending on the host's settings.

Using one shared service identity would make that identity the guest. It does not
establish that the visitor receives the invitation or that their calendar becomes
busy. Extracting identity from a supplied public URL also does not authenticate
the submitter as its owner.

Before implementation, decide which identity books, whether missing details may
be requested, and how to verify that both people's calendars reflect the meeting.
Do not introduce these fields or a shared mailbox during the foundation stage.

## Error cases for the future booking flow

- Invalid, unsupported, deleted, or inaccessible Google booking page.
- No provable common availability within the search window.
- Missing guest information or required verification.
- Calendar source unavailable or its internal protocol changed.
- Slot lost to another booking.
- Anakin rejection, timeout, or outcome requiring reconciliation.

Keep implementation details out of the user interface. Do not expose provider
payloads, credentials, other calendar events, or internal exception messages.

## Current browser preview

The three routes are now available as a mock test, ahead of calendar integration.
The shared page says “30-minute test meeting” and offers “Find a test time”.
It displays a simulated common interval in the visitor’s time zone, a no-match
result, or an actionable error. A permanent test badge and outcome copy state
that nothing is booked. Appointment metadata and availability are not read from
Google yet. The final booking behavior specified above remains a later gate.
See [browser test](browser-test.md).

## Historical stage 1 scope

- A healthy production API: `GET https://api.calendar.aithos.world/health`.
- A blank HTML document at `https://calendar.aithos.world/`.
- The infrastructure and deployment workflow required to deliver both.

At stage 1, the three product pages were specifications only. Stage 1 had no inputs,
calendar reads, Anakin account, real bookings, A2A runtime, or application database.

## References

- [Create an appointment schedule](https://support.google.com/calendar/answer/10729749)
- [Google email verification](https://support.google.com/calendar/answer/11902347)
- [Anakin Google appointments actions](https://anakin.io/catalog/google_appointments)
