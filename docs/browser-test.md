# Browser test: public links and mock A2A

The website reproduces the terminal test using the real production onboarding,
discovery and A2A services. Availability is simulated; nothing is reserved.

## Manual acceptance

1. Open [Calendar](https://calendar.aithos.world/).
2. Paste a public Google appointment booking-page URL and choose **Create link**.
   A saved page reuses its existing agent. A newly published card may take a few
   seconds; the spinner remains visible while publication is retried.
3. Copy the returned link, then open it in a new browser tab or use **Open link**.
   Direct navigation and refresh must work.
4. Paste a **different** public Google booking-page URL and choose **Find a test time**.
5. Expect **A shared test time was found**, January 15, 2030, 09:30–10:00 UTC
   converted to the browser’s time zone (10:30–11:00 in Europe/Paris). The page
   explicitly says that the times are simulated and nothing was booked.
6. Expand **Test details** to obtain the trace ID for [CloudWatch logs](logging.md).
   The backend performs the same host → peer exchange as the CLI.

Also check:

- An unsupported URL produces a useful error and allows another submission.
- Supplying the host’s own booking page reports that a different page is needed.
- `/book/unknown-agent` displays an unavailable-link message.
- The help link opens the tutorial; `/help/google-booking-page` also works directly.
- Inputs and the action are disabled while processing; old results are replaced.

All newly created page agents currently expose the same 30-minute mock interval.
A normal two-page browser test therefore finds a match. The CLI’s 60-minute case
remains useful for testing no-match against production. The web preview fixes
the test duration at 30 minutes; real host metadata is a later gate.

## Minimal implementation

One static HTML file contains the three pages, CSS and a small JavaScript module.
There is no frontend framework or package installation. API Gateway permits
browser calls from the Calendar site; CloudFront serves the same file for deep
links. No AWS or registry credentials enter the browser.

The browser loads the host’s signed card from Calendar’s identical local card
route, validates its endpoint and tenant, then sends `SendMessage` JSON-RPC to
`/a2a`. The existing Rust service resolves the peer through the catalog and Aithos
and makes the real SDK client call. Browser code does not implement discovery
or scheduling. It checks participants, trace, mock status and interval before
displaying success.

Publication retries are limited to five attempts within a 45-second action
budget. Only `pending` responses are automatically retried, using the same URL.
The A2A request is sent once. After a timeout, a user can resubmit the URL safely
under the existing onboarding contract. This policy applies to this mock test;
real booking will require durable operations and outcome reconciliation.

## Isolated browser checks

To exercise UI states without contacting Google, Aithos or AWS:

```sh
python3 scripts/preview-web.py --pending-once
```

Open `http://127.0.0.1:3189/`. Use `https://calendar.app.google/host` on the home
page and `https://calendar.app.google/guest` on the shared page. These are local
fixture inputs only. The preview server substitutes the API origin in its served
copy; it does not alter the production file.

Use `--result no_common_slot`, `--result error`, or `--result invalid_response`
to exercise other outcomes. Choose `--port 3190` to run a separate scenario.
`invalid_response` claims a reservation and must be rejected by the UI.

Local browser verification covered loading, pending publication retry, sharing,
success, unsupported links, same-page rejection, no match, invalid results,
unknown agents, and the 390-pixel mobile layout.

CI checks the inline JavaScript syntax, retains the Rust/A2A checks, and runs
`scripts/smoke-web.py` after deployment to verify deep links and CORS.
