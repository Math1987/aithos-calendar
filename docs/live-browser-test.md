# Gate 5: real availability in the browser

## Scope

The shared page displays the host's real title and appointment duration. A visitor
pastes their public Google booking page; their agent is created or reused. The host
agent reads its page while discovering the visitor through the catalog and signed
Aithos AgentCard, then asks for availability through A2A. The same runtime handles
both tenants. There is no LLM, Bedrock, booking API call or calendar write.

The result is the earliest **complete host appointment** covered by the visitor's
advertised intervals in the next 30 days. Touching visitor intervals can cover a
longer host appointment; gaps cannot. Public pages are not a complete free/busy
API: only advertised coverage counts, and busy calendars must be included in the
Google appointment schedule's availability settings. No matching offered slot
means no match within this scope, not that the people can never meet.

## Deployment and existing cards

Push the reviewed implementation to main through the existing CI/CD pipeline.
Terraform adds `GET /agents/{tenant}/schedule` to the existing API integration,
raises Lambda timeout to 25 seconds and API timeout to 29 seconds. Live comparison
has a 15-second application deadline; reads are bounded at 8 seconds. Health stays
independent of Google. No new AWS resource or runtime IAM permission is needed.

New page agents publish card version `0.4.0`. Existing `0.3.0` page agents require
this operator-only migration using AWS credentials able to read the recovery key:

```sh
python3 scripts/with-env.py cargo run --locked --example upgrade_cards -- \
  check calendar-production-agents https://api.calendar.aithos.world https://registry.aithos.world
python3 scripts/with-env.py cargo run --locked --example upgrade_cards -- \
  apply calendar-production-agents https://api.calendar.aithos.world https://registry.aithos.world
```

Review `check` before `apply`. The migration only upgrades page-bound 0.3.0 agents;
it keeps tenant, registry ID, URLs and signing key, clears fixture slots, signs a
0.4.0 card and verifies its public bytes before conditionally replacing the local
record. The runtime cannot read recovery keys. If registry caching returns
`registry_not_ready` or `registry_card_mismatch`, rerun the same command: the signed
card bytes are deterministic. Concurrent local record changes cause the conditional
update to fail; inspect those before retrying. Fixture agents without a page stay
mocked. The browser rejects old mock links until upgraded.

## Manual browser acceptance

1. Open https://calendar.aithos.world/. Confirm **Live availability · no bookings**.
2. Paste the host page, create the link, and open it:
   `https://calendar.app.google/e5GJSbH72D11kYk3A`.
3. Confirm the meeting title and duration agree with Google's public page.
4. Paste the visitor page: `https://calendar.app.google/dRgnindyWDsffm8z8`.
5. Click **Find a time**. Check the spinner, then the proposed date/time and named
   local time zone. The result must state that no appointment was booked.
6. Open both Google booking pages and check that the proposed host appointment is
   offered and fully covered by the visitor's availability.
7. Reverse the two pages. The new host's duration and offered starts are authoritative.
8. On a controlled calendar, mark the proposed interval busy (in a calendar checked
   by that appointment schedule). Repeat. The busy interval must disappear; compare
   with Google's own page if a setting or propagation delay is in doubt.
9. With controlled schedules, test unequal durations and a one-minute coverage gap.
   The host duration wins; a gap must not be crossed. Give the schedules disjoint
   advertised availability and confirm **No shared time found**.
10. Paste the same page or an invalid link, and open `/book/unknown-agent` directly.
    Each must show a useful error without reporting success. Reload and retry.

No appointment should appear as a consequence of these tests. The next gate adds
one controlled Anakin booking, verified confirmation and durable operation state.

## CLI and logs

Use the same catalog/card lookup as before. On a live host agent, omit duration:

```sh
DATA=$(jq -cn --arg peer "$PEER" '{operation:"find_common_slot",peer:$peer}')
.build/tools/a2acli --agent-card "$CARD" -o json send --data-part "$DATA"
```

Expect `mock:false`, `reserved:false`, the host `duration_minutes`, a 30-day search
window, and `slot_found` or `no_common_slot`. An explicitly supplied duration must
match the current host appointment duration. A Google/discovery failure is an error,
never an empty successful schedule or fallback to mock data.

Use the result's trace ID with the existing [logging guide](logging.md). The peer
call should be `get_availability` with the same bounded window. Names/emails used
for booking are omitted from availability, metadata, cards and ordinary logs.

## Automated acceptance

The Rust suite covers live A2A with different durations, contiguous coverage, gaps,
Google failure, stale requested duration, missing peer, old mock rejection and
contact omission. Registry verification checks upgrade signatures, version order,
key continuity and deterministic retry bytes. The browser preview can exercise
success, no match and error independently of Google:

```sh
python3 scripts/preview-web.py --result slot_found
# Alternatives: --result no_common_slot, --result error, --result invalid_response
```

These isolated preview responses are fixtures; use the deployed website and the
real Google pages for owner acceptance.

## Verification on 2026-09-17

- 31 offline tests passed, including live-mode HTTP/A2A collaboration and independent
  registry verification of the card upgrade.
- An explicit read-only integration test used both real Google pages with a local
  registry and real A2A HTTP. Both directions returned `slot_found`, 30 minutes,
  `mock:false`, `reserved:false`. No public registry was modified by this test.
- Isolated browser checks verified creation of a share link, meeting metadata,
  loading state, a successful match, no match, and rejection of the same page.
- JavaScript syntax and Terraform validation passed.
- Production deployment/card migration remain pending: operator AWS credentials
  returned `ExpiredToken`. The existing production website remains on mocks.
