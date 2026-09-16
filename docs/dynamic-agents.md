# Gate 4 — persistent identities and Aithos publication

Status: implementation and local acceptance complete; production deployment pending.

## Scope and decisions

This gate creates operator-managed mock agents through an AWS IAM protected API.
It does not introduce end-user accounts or prove ownership of a Google calendar.
A public booking URL is not an ownership credential. End-user onboarding and
account recovery remain explicit product decisions before opening creation to visitors.

- **One runtime:** all agents still use the same Rust Lambda and `/a2a` endpoint.
- **Persistence:** one private DynamoDB table, on-demand billing, encryption at rest,
  point-in-time recovery and deletion protection. No database server or worker.
- **Administration:** API Gateway verifies AWS SigV4 (`AWS_IAM`). The application
  also requires the trusted Lambda request context to identify a caller from the
  configured AWS account. HTTP headers cannot substitute for that context.
- **Ownership:** the IAM user/root ARN or assumed-role ARN without its session
  suffix owns the record. Sessions of the same role share ownership; separate
  roles/users cannot read admin status or replay publication for that record.
  This is operator/role ownership, not individual visitor authentication.
- **Immutable definitions for this gate:** name and mock intervals are fixed at
  creation. Repeating the same definition is safe; different content under the
  same Calendar ID returns 409. Editing, deleting and rotating keys have no API yet.

## Aithos integration: actual deployed contract

On September 16, 2026, `https://registry.aithos.world/v1/registry` and
`/v1/agents` were available; `/.well-known/ai-catalog.json` returned 404.
Aithos publishes signed card bytes, rather than simply storing our URL.
The registry worktree contains ongoing unrelated work and was not modified.

Calendar therefore retains its standard AI Catalog endpoint and fills it from
**successfully published** local records. Each entry's URL points to the signed
card hosted by Aithos. Calendar also serves identical signed bytes at its own
`/agents/{id}/agent-card.json` URL. This preserves signatures on both copies.
The catalog projection can later move to Aithos without changing the A2A client.

Discovery:

```text
Calendar AI Catalog → exact Aithos card URL → SDK client → Calendar /a2a + tenant
```

Only the configured Calendar and Aithos origins may serve cards. Message endpoints
must still use Calendar's own origin; redirects remain disabled. No arbitrary
external agent or URL supplied by a caller gains access to the network.

Two identifiers have distinct roles:

- **Calendar ID:** a client-chosen UUID, retained before the first request. It is
  the runtime tenant and the suffix of `urn:aithos:calendar:agent:{id}`.
- **Aithos ID:** the RFC 7638 thumbprint of the generated genesis public key. It
  names the registry entry; the card itself routes to the Calendar UUID.

## Publication and retries

1. Validate the caller, UUID, name and mock intervals.
2. Generate a P-256 key and sign the card with ES256. Use the published
   `aithos-a2a-card` crate for strict presence validation and canonical bytes.
3. Sign Aithos's separate publication proof, bound to its origin, the Aithos ID
   and the exact card digest. A card signature alone does not authorize publication.
4. Atomically persist the definition, signed card, proof, owner and recovery key
   **before** contacting Aithos. A conditional insert chooses one winner under races.
5. PUT the stored publication envelope to Aithos; verify the public card bytes.
6. Mark the record published, enabling its catalog entry, public card and A2A handler.

A lost response or an unavailable registry leaves a durable pending record.
Replay reuses the same identity, signed bytes and proof. No background work runs
after the HTTP response. The published registry contract accepts identical bytes
idempotently. Already confirmed records do not cause another registry write.

The per-agent recovery signing key is a separate DynamoDB attribute. Runtime
GetItem/Scan projections and their IAM policy exclude it; it is never returned by
an API, logged, committed, or put in Terraform state. DynamoDB encrypts it at rest;
it is not separately envelope-encrypted by Calendar. The service is the signing-key
custodian in this gate. Authorized AWS recovery operators can recover that attribute;
protect the table and its backups. The registry itself never receives private keys.

Registry calls have a 3-second HTTP timeout. DynamoDB operations have a 2-second
overall budget, with at most two SDK attempts. Administration has a 12-second
application deadline within the existing 15-second Lambda timeout. A timeout can
occur after a write: reuse the UUID and check status, rather than creating a new one.

## API

All admin routes require AWS IAM and the same record owner:

| Request | Behavior |
| --- | --- |
| `PUT /admin/agents/{id}` | Create, or replay the same definition; attempt publication |
| `GET /admin/agents/{id}` | Read publication status and public definition |
| `POST /admin/agents/{id}/publish` | Resume a pending publication |

Body for PUT:

```json
{"name":"My mock agent","mock_availability":[{"start":"2030-01-15T09:00:00Z","end":"2030-01-15T10:00:00Z"}]}
```

Names are nonblank, trimmed, at most 80 bytes, without control characters.
There must be 1–32 intervals with start before end. Dates are parsed as UTC instants.
The body limit is 16 KiB. Successful publication returns 200; saved but pending
publication returns 202. Invalid input is 400 (or the JSON extractor's 422), wrong
owner/anonymous caller 403, conflicting definition 409, unavailable storage 503.
A 504 with `operation_incomplete_retry_same_id` means the operation is uncertain.

Public discovery and A2A remain anonymous because only mock availability is exposed.
A pending agent is not public. The pilot catalog fails explicitly rather than
truncating when it exceeds 64 KiB or the store scan exceeds 1,000 records.
Pagination and large-scale listing are future work.

## Manual acceptance

Operator helper requirements: Python 3 and `botocore` (already available on the
workstation). It uses AWS SDK SigV4 and does not print credentials or signed headers.
Run with `scripts/with-env.py`, or with a valid AWS profile. The caller must have
`execute-api:Invoke` for this API's admin routes.

```sh
cd "/Volumes/Math17/aithos/R&D/calendar"
HOST_ID=$(python3 -c 'import uuid; print(uuid.uuid4())')
GUEST_ID=$(python3 -c 'import uuid; print(uuid.uuid4())')
# Retain both IDs before sending requests; never regenerate one to retry.

python3 scripts/with-env.py python3 scripts/manage-agents.py create \
  --id "$HOST_ID" --file examples/gate-4-host.json
python3 scripts/with-env.py python3 scripts/manage-agents.py create \
  --id "$GUEST_ID" --file examples/gate-4-guest.json
```

If a response reports `pending`:

```sh
python3 scripts/with-env.py python3 scripts/manage-agents.py status --id "$HOST_ID"
python3 scripts/with-env.py python3 scripts/manage-agents.py publish --id "$HOST_ID"
```

Discover the host through the catalog, then ask it to find time with the guest:

```sh
CATALOG_URL=https://api.calendar.aithos.world/.well-known/ai-catalog.json
HOST_CARD=$(curl -fsS "$CATALOG_URL" | jq -er --arg id "urn:aithos:calendar:agent:$HOST_ID" '.entries[] | select(.identifier == $id) | .url')
.build/tools/a2acli --agent-card "$HOST_CARD" -o json send \
  --data-part "{\"operation\":\"find_common_slot\",\"peer\":\"urn:aithos:calendar:agent:$GUEST_ID\",\"duration_minutes\":30}"
```

The examples return 2030-01-15 09:30–10:00 UTC, `mock: true`, `reserved: false`.
Reverse the caller/peer to test the other direction. Use 60 minutes for no common
slot. Rerun create with the same ID/file to verify idempotence. A changed name under
the same ID must return 409; an unsigned request must return 403.

`scripts/smoke-a2a.py` now reads dynamic catalog entries and their advertised mock
availability. It verifies greetings, absent tenants, both peer directions and
missing peers. An empty initial catalog passes readiness only; it does not claim
to have verified collaboration. Full gate acceptance requires two published agents.

Alice and Bob remain **local test fixtures only**. The Lambda always uses DynamoDB;
there is no production fixture fallback. Existing `/agents/alice/…` and `/agents/bob/…`
URLs will return 404. `CALENDAR_LISTEN` still runs isolated local fixtures.

## Validation record

Local: 18 tests pass. Three new integration tests exercise independent Aithos
signature/proof verification, a committed publication whose response is lost,
retry identity stability, concurrent creation, ownership isolation, immutable
conflicts, public visibility, signed-byte preservation and bidirectional discovery
through registry-hosted cards. Existing interval, routing and logging tests remain.

Bootstrap: reviewed and applied 2 IAM policies, 0 changes, 0 deletions. Both policies
are scoped to `calendar-production-agents`; production CI still cannot change IAM.
Production deployment and manual acceptance pending.
