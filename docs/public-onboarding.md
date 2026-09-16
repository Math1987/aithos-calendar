# Public onboarding: one Google booking page, one agent

Status: implementation and local acceptance complete; production verification pending.
This supersedes the operator-only creation API from the first Gate 4 deployment.

## Contract

```http
POST /agents
Content-Type: application/json

{"booking_page_url":"https://calendar.app.google/YOUR_LINK"}
```

No login, AWS credentials, management token or Google OAuth is required. Submitting
an already known page reuses its agent; it never edits its definition. Possession
of a public page URL does not prove ownership. The agent represents the **booking
page**, not an authenticated person. Different pages belonging to one person
produce different agents.

The response contains `id`, `identifier`, `booking_page_url` (canonical),
`agent_card_url`, `registry_id`, `share_url`, `publication_status`, `mock` and
`reserved`. Names and mock availability are server-defined. Additional input
fields (including tenant, owner, name and availability) are rejected.

- **200 / published:** card publication is confirmed; the agent is in the catalog.
- **202 / pending:** record saved; repost the same URL to resume publication.
- **400:** unsupported URL or destination.
- **422:** invalid JSON definition, or Google's HTML is not recognized as a public
  booking page. The latter includes nonexistent schedules returning Google's generic
  HTTP 200 shell; it can also mean Google's markup changed.
- **413:** request body exceeds 4 KiB.
- **503:** Google or storage unavailable; retry the same URL later.
- **504:** operation deadline exceeded; it may have saved data. Repost the same URL.

The returned `share_url` reserves the stable `/book/{id}` address. **The website is
still blank and the booking UI is not implemented.** Test the API, cards and A2A
with the terminal for now. All availability remains the fictitious January 15,
2030, 09:30–10:00 UTC interval for new page agents, with `mock: true` and
`reserved: false`. No Google availability or actual appointment is read or booked.

## Minimal implementation

1. Resolve a supported short link and normalize the schedule URL.
2. Derive the Calendar ID as the lowercase hexadecimal SHA-256 of
   `calendar:google-booking-page:v1:` followed by that canonical URL.
3. Read the existing DynamoDB record by that ID. If present, reuse its exact card,
   key identity and publication proof. Already confirmed cards are not republished.
4. For a new page only, check Google's public HTML, generate/sign the card and
   conditionally insert with `attribute_not_exists(id)`.
5. A concurrent loser loads the winning record. Only the winning card is published.
6. Publish to Aithos, verify its public bytes, and mark the record published.

One table and one item per booking page suffice: no secondary index, mapping table,
lock row or transaction. The ID is deterministic, not a secret or an ownership
credential. The Aithos ID remains the distinct fingerprint of the generated public
key. A defensive canonical-URL comparison rejects a hash collision or inconsistent
record instead of changing it.

The old UUID mock agents remain readable with their original signed cards. They
contain no booking-page association, so they cannot be matched to a Google URL.
There is no data migration or deletion. Old `/admin/agents/...` routes and the
AWS-signing helper are removed. Lambda still uses IAM internally for DynamoDB.

## Google adapter and limits

`BookingPages` separates identity resolution from validation of a newly seen page.
`GoogleBookingPages` implements the current HTTP behavior. A future provider or
parser can replace it without changing identity storage or publication.

Supported HTTPS URLs:

- `calendar.app.google/{short-code}`
- `calendar.google.com/appointments/schedules/{schedule-id}`
- `calendar.google.com/calendar/appointments/schedules/{schedule-id}`
- `calendar.google.com/calendar/u/{numeric-account}/appointments/schedules/{schedule-id}`

Canonical form is the third variant. Queries, fragments, account selector and
trailing slash do not change identity. IDs are case-sensitive ASCII letters,
digits, underscores and hyphens. Percent-encoded IDs, ICS feeds, Calendar home
pages and other Google sharing formats are not supported.

Every redirect is checked before following it: exact allowlisted hosts, HTTPS,
port 443, recognized paths, no credentials. No cookies or user authorization are
forwarded. At most four requests per phase, three seconds per HTTP request, five
seconds per resolution/validation phase and twelve seconds for onboarding.
HTML is capped at 512 KiB. The current recognition checks canonical identity,
AppointmentsInitialData and the booking-page Open Graph image. This is an
undocumented Google HTML adapter, not an official API or proof of ownership.
A changed or missing marker fails closed without creating an agent.

Existing canonical long URLs reuse stored records without contacting Google.
Short links still need Google to resolve them; if that fails, resubmit the saved
canonical URL. Removing or changing a Google page does not automatically revoke
an existing Aithos identity; lifecycle management remains future work.

## Manual terminal test — no credentials needed

```sh
cd "/Volumes/Math17/aithos/R&D/calendar"
mkdir -p .build
API=https://api.calendar.aithos.world
BOOKING_URL='https://calendar.app.google/e5GJSbH72D11kYk3A'

jq -n --arg url "$BOOKING_URL" '{booking_page_url:$url}' > .build/onboarding-request.json
curl --fail-with-body -sS "$API/agents" \
  -H 'Content-Type: application/json' \
  --data-binary @.build/onboarding-request.json | tee .build/my-agent.json | jq .
```

If `pending`, wait a few seconds and repeat **the same curl command**. Aithos may
accept a write before its public card is readable. Continue once `published`.

### Reuse the long URL

```sh
LONG_URL=$(jq -r '.booking_page_url' .build/my-agent.json)
jq -n --arg url "$LONG_URL?gv=true" '{booking_page_url:$url}' > .build/onboarding-long.json
curl --fail-with-body -sS "$API/agents" \
  -H 'Content-Type: application/json' \
  --data-binary @.build/onboarding-long.json > .build/same-agent.json

diff <(jq -S '{id,registry_id,agent_card_url,share_url}' .build/my-agent.json) \
     <(jq -S '{id,registry_id,agent_card_url,share_url}' .build/same-agent.json)
```

Expect no difference. Reposting the URL is also the publication retry mechanism;
there is no separate public edit endpoint.

### Discover and call it

```sh
ID=$(jq -r '.id' .build/my-agent.json)
CARD=$(curl -fsS "$API/.well-known/ai-catalog.json" | jq -er \
  --arg id "urn:aithos:calendar:agent:$ID" '.entries[] | select(.identifier == $id) | .url')
.build/tools/a2acli --agent-card "$CARD" send 'Hello'
.build/tools/a2acli --agent-card "$CARD" -o json send \
  --data-part '{"operation":"get_availability"}'
```

To test collaboration, select an existing different agent from the catalog. You
can also submit a **different real Google booking page** to create a second one.

```sh
PEER=$(curl -fsS "$API/.well-known/ai-catalog.json" | jq -er \
  --arg self "urn:aithos:calendar:agent:$ID" '[.entries[] | select(.identifier != $self)][0].identifier // empty')
DATA=$(jq -cn --arg peer "$PEER" '{operation:"find_common_slot",peer:$peer,duration_minutes:30}')
.build/tools/a2acli --agent-card "$CARD" -o json send --data-part "$DATA"
```

The deployed Gate 4 mock peers overlap the new fixed interval. Expect `slot_found`,
09:30–10:00 UTC on January 15, 2030, and `reserved: false`. Request 60 minutes to
obtain `no_common_slot`.

### Invalid input

```sh
curl -sS -w '\nHTTP %{http_code}\n' "$API/agents" \
  -H 'Content-Type: application/json' \
  -d '{"booking_page_url":"https://example.com/not-google"}'
```

Expect HTTP 400. Unknown input fields must return 422 and cannot modify an agent.

### Concurrent submissions

Run the same long-URL request simultaneously in two terminals, or:

```sh
curl --fail-with-body -sS "$API/agents" -H 'Content-Type: application/json' \
  --data-binary @.build/onboarding-long.json > .build/concurrent-1.json &
curl --fail-with-body -sS "$API/agents" -H 'Content-Type: application/json' \
  --data-binary @.build/onboarding-long.json > .build/concurrent-2.json &
wait
jq '{id,registry_id,share_url}' .build/concurrent-{1,2}.json
```

Expect identical identity fields. To exercise concurrent **first** creation,
prepare the requests using a previously unsubmitted real page. The automated
integration test forces that race before either database insert.

## Validation

The deterministic suite covers anonymous creation, alias identity, safe URL and
redirect filtering, unknown fields and oversized bodies, publication loss/retry,
unchanged signed bytes, a forced concurrent first-creation race, Aithos signatures
and proofs, and real bidirectional A2A through registry-hosted cards.

The optional read-only provider check runs separately:

```sh
GOOGLE_BOOKING_TEST_URL="$BOOKING_URL" cargo test --locked --test google_live -- --ignored
```

Production acceptance will be recorded after deployment.
