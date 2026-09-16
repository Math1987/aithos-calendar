# Gate 3 — mock agent-to-agent scheduling

Status: gate 3 was verified on September 16, 2026. Gate 4 replaces production Alice/Bob fixtures with [dynamic identities](dynamic-agents.md); commands below remain valid for local fixtures.

## Behavior

The CLI addresses Alice with `find_common_slot`, Bob's catalog identifier and a
whole-minute duration. Alice fetches the configured catalog over HTTP, selects
Bob's URL entry, fetches that exact Agent Card URL, and builds a client using the
official `a2a-client-lf` 0.2.5 SDK. The SDK selects JSON-RPC and copies the card's
tenant into its outbound request. Alice sends only `get_availability` to Bob.
Bob returns fixture availability; Alice computes the earliest complete common
interval and returns an immediate A2A Message. The same code supports Bob → Alice.

In AWS, the outbound request reaches API Gateway and invokes the same Lambda
function again for the recipient. There is no direct in-process call to Bob's
availability from Alice's coordination logic. No recursive coordination message
is sent, no task store is needed, and no work continues after the response.
The deployment must allow concurrent invocations; do not limit it to one.

Plain text retains the previous greeting behavior. Instructions use exactly one
A2A data part; natural-language interpretation has not been implemented.

## Fixed mock calendar

All intervals below are fictitious and dated **January 15, 2030, UTC**:

| Agent | Available intervals |
| --- | --- |
| Alice | 09:00–10:00; 14:00–15:00 |
| Bob | 09:30–10:30; 15:00–16:00 |

A 30-minute request yields 09:30–10:00. A 60-minute request yields no common slot.
The requested duration must be a whole number from 1 to 480 minutes. ProtoJSON
represents data-part numbers as doubles, so 30 and 30.0 are accepted equally;
fractional values are rejected. Timestamps are parsed and compared as UTC instants.
The entire requested interval must fit both calendars. Touching boundaries do not
constitute available time. Fixtures are never read from Google.

## Configuration and boundaries

- `CALENDAR_PUBLIC_URL`: canonical origin serving this deployment's agents.
- `CATALOG_URL`: server-side discovery URL; defaults locally to the public origin's
  `/.well-known/ai-catalog.json`. Terraform explicitly sets it in production.
- `CALENDAR_LISTEN`: optional local HTTP listener; absent in Lambda.

Changing the CLI's `CATALOG_URL` variable changes where **the CLI** discovers its
initial agent. Changing **Alice's** discovery source requires updating the Lambda
configuration through Terraform. Later the server-side URL can point to Aithos's
AI Catalog, while the cards and A2A endpoint stay on Calendar. No registry writes
or publication were added. Explicit card URLs remain unchanged; the SDK resolver
improvement discussed separately is not required for this gate.

Catalog and card retrieval use a small HTTP adapter and the existing AI Catalog
and A2A types. A2A messaging uses the SDK factory and transport. Discovery allows
only URL entries of type `application/a2a-agent-card+json`, with unique identifiers.
For this fixture gate, cards and message endpoints must remain on the configured
agent origin. The catalog itself may be hosted elsewhere. Redirects are disabled;
credentials in URLs are rejected; discovery documents are limited to 64 KiB.
External agent origins will require an explicit trust policy at a later gate.

Request budgets: connection 2 seconds, each HTTP request 3 seconds, outbound A2A
availability call 2.5 seconds, full discovery/exchange 10 seconds, Lambda 15 seconds,
API Gateway integration 20 seconds. No automatic retry is introduced. The SDK's
TLS backend supports HTTPS calls from the existing Linux musl binary.

Peer availability is checked for agent identity, matching trace ID, mock status,
valid intervals and bounded interval count before matching. A Task response or
unexpected payload is rejected because this gate requires an immediate Message.

## Response contract

Responses include readable text plus a data part:

- `status: slot_found` with `slot.start` and `slot.end`.
- `status: no_common_slot` with `slot: null`.
- `status: error` with `code`: `peer_not_found`, `discovery_unavailable`,
  `invalid_peer_card`, `peer_unavailable`, `invalid_peer_response`, or `timeout`.

Every coordination outcome includes `trace_id`, `mock: true` and `reserved: false`.
The CLI exits 0 when it successfully receives these business outcomes, including
`status: error`; inspect the data status. Invalid commands/durations, unknown
recipient tenants and unsupported A2A methods use protocol errors instead.
No booking, hold, invitation, calendar access, LLM, memory or user authentication
is involved. Neither the tenant nor trace ID authenticates the caller.

## Manual CLI acceptance

The workstation has the official CLI 0.2.1 at `.build/tools/a2acli`. Start
`CALENDAR_LISTEN=127.0.0.1:3187 cargo run --locked` in another terminal first:

```sh
cd "/Volumes/Math17/aithos/R&D/calendar"
export PATH="$PWD/.build/tools:$PATH"
export CATALOG_URL="http://127.0.0.1:3187/.well-known/ai-catalog.json"
ALICE_CARD=$(curl -fsS "$CATALOG_URL" | jq -er '.entries[] | select(.identifier == "urn:aithos:calendar:agent:alice") | .url')
BOB_CARD=$(curl -fsS "$CATALOG_URL" | jq -er '.entries[] | select(.identifier == "urn:aithos:calendar:agent:bob") | .url')
```

### Alice discovers and asks Bob

```sh
a2acli --agent-card "$ALICE_CARD" -o json send --data-part   '{"operation":"find_common_slot","peer":"urn:aithos:calendar:agent:bob","duration_minutes":30}'
```

Expect `slot_found`, 2030-01-15 09:30–10:00 UTC and `reserved: false`.
The relevant data is under `message.parts[].data`; use
`jq '.message.parts[] | select(has("data")) | .data'` to extract it.

### Reverse direction

```sh
a2acli --agent-card "$BOB_CARD" -o json send --data-part   '{"operation":"find_common_slot","peer":"urn:aithos:calendar:agent:alice","duration_minutes":30}'
```

Expect the same slot, with Bob as organizer and Alice as peer.

### No common slot and absent peer

```sh
a2acli --agent-card "$ALICE_CARD" -o json send --data-part   '{"operation":"find_common_slot","peer":"urn:aithos:calendar:agent:bob","duration_minutes":60}'
a2acli --agent-card "$ALICE_CARD" -o json send --data-part   '{"operation":"find_common_slot","peer":"urn:aithos:calendar:agent:missing","duration_minutes":30}'
```

Expect `no_common_slot`, then `status: error` with `code: peer_not_found`.
To inspect Bob's leaf operation directly:

```sh
a2acli --agent-card "$BOB_CARD" -o json send --data-part '{"operation":"get_availability"}'
```

## Tracing the real exchange

Take `trace_id` from the returned data. CloudWatch records application JSON events
with `target`, `span.tenant` and `span.trace_id`:

1. `operation_received`, tenant Alice, operation `find_common_slot`.
2. `peer_call`, caller Alice, recipient tenant Bob.
3. `operation_received`, tenant Bob, operation `get_availability`.
4. `negotiation_completed`, tenant Alice, outcome status.

Native SDK request/response/error events share that context. See the
[logging guide](logging.md) for source filters and the server adapter scope.

The shared trace appears under different AWS request IDs/log streams when Alice
and Bob run in separate invocations. A supplied valid UUID in request metadata
`calendarTraceId` is propagated for correlation only. Logs do not contain full
request bodies, availability payloads or credentials.

```sh
python3 scripts/with-env.py aws logs tail /aws/lambda/calendar-production-health --since 10m --format short
```

## Tests and deployment checks

- 2 interval tests: normalization, ordering, full-duration fit, touching/empty intervals.
- 8 real HTTP integration tests: custom catalog/card URLs, SDK-selected tenants,
  both directions, trace propagation, exactly one leaf call, absent/unavailable
  peers, invalid payloads/intervals, timeout, disallowed origin and invalid commands.
- 4 existing discovery/greeting/error tests retained.
- 1 binary-level JSON logging test checks SDK/application sources and concurrent trace isolation.
- `scripts/smoke-a2a.py CATALOG_URL` verifies greeting behavior, both collaboration
  directions, no overlap and an absent peer against the running deployment.

Run `cargo test --locked` locally. For manual local CLI tests, run
`CALENDAR_LISTEN=127.0.0.1:3187 cargo run --locked` and change the CLI catalog URL to
`http://127.0.0.1:3187/.well-known/ai-catalog.json`. The unavailable/slow/malformed
peer scenarios are exercised by isolated local test servers, without disrupting
production or exposing public fault-injection controls.

## Verification record

Verified on **September 16, 2026**:

- Application commit: `5466988212535c50cbf8ddea7acf241c2424a0e8`.
- [Production workflow](https://github.com/Math1987/aithos-calendar/actions/runs/35062089693): successful; 14 Rust tests passed, Linux x86-64 static binary built, health and A2A smoke checks passed.
- Terraform: **0 added, 2 changed, 0 destroyed**. Only the Lambda and API integration changed; no new AWS resources or permissions.
- Official CLI 0.2.1, with card URLs selected from the public catalog: Alice → Bob and Bob → Alice returned `slot_found` for 2030-01-15 09:30–10:00 UTC. A 60-minute request returned `no_common_slot`; an absent peer returned `error` / `peer_not_found`.
- All coordination responses retained `mock: true` and `reserved: false`.
- `/health` returned the expected JSON; the HTTPS website returned 200 with an empty body element.

CloudWatch confirmed the real Alice → Bob exchange under trace
`01a0a8d6-532a-7243-ab2a-526ca6bb2cef`:

| Event | Tenant | Execution environment (log stream suffix) |
| --- | --- | --- |
| `operation_received: find_common_slot` | Alice | `fcfd93fa1e5e4590945896671fb4f98c` |
| `peer_call` | Alice → Bob | `fcfd93fa1e5e4590945896671fb4f98c` |
| `operation_received: get_availability` | Bob | `04235c033722447d9541dde4901ed9b3` |
| `negotiation_completed: slot_found` | Alice | `fcfd93fa1e5e4590945896671fb4f98c` |

The distinct execution environments confirm that Bob handled a separate Lambda
invocation while Alice awaited his answer. Catalog and card GET requests also
invoke the service; the table tracks only the two agent operations.
