# Calendar — an auditable A2A + AI Catalog trust proof of concept

Two people connect their Google Calendars; their agents discover each other
through an [AI Catalog](https://ai-catalog.io/) and negotiate a meeting over
[A2A 1.0](https://a2a-protocol.org/). Nothing in this repository is novel as
a scheduler. What it demonstrates, end to end and verifiably, is **trust**:

- every A2A Agent Card is **signed** by its agent's key and served from this
  deployment with the key set that verifies it;
- the AI Catalog is **Level 3 "Trusted"**: every entry carries a
  guarantor-signed `trustManifest` bound to the exact card bytes, the host
  carries its own manifest, and the document itself is signed;
- the discovery client **verifies the whole chain before any call**
  (catalog signature → manifest → card digest → card JWS), under an explicit
  per-operation policy, with one error code per failed step;
- the called agent **authenticates the caller** with a signed request
  (a prototype, since neither specification covers it);
- a **scenario lab** serves deliberately broken catalogs any client can be
  pointed at, and a **public log feed** shows every verification step live.

Repository: https://github.com/Math1987/aithos-calendar. Everything is
written in English; the trust layer is `src/trust/`, its reference is
[`docs/trust-layer.md`](docs/trust-layer.md), and the evaluation written
for the specification maintainers is
[`docs/trust-manifest-evaluation.md`](docs/trust-manifest-evaluation.md).

## The chain

```
                     operator key (KMS)              guarantor key (KMS)             agent key (per agent)
                     /.well-known/jwks.json          /trust-provider/.well-known/    /agents/{id}/jwks.json
                            │                              jwks.json                        │
                            ▼                                  │                            ▼
  /.well-known/ai-catalog.json ── signature ──┐                ▼                 /agents/{id}/agent-card.json
    host.trustManifest  ─────── signature ────┤    entries[].trustManifest          signatures[0] (JWS + JCS)
    entries[] ────────────────────────────────┘      subject.digest = sha256(card bytes)
                                                     subject.url    = card url

  client:  fetch catalog → verify signature (operator, same origin)
        →  select entry → verify manifest (pinned guarantor, window, subject)
        →  fetch card → sha256 == subject.digest → verify card JWS (agent key at jku)
        →  policy attestation (booking) → create_from_card → SendMessage (+ Agent-Signature)
```

| What | Where |
| --- | --- |
| Catalog (Level 3, `application/ai-catalog+json`) | `https://api.calendar.aithos.world/.well-known/ai-catalog.json` |
| Operator key set (catalog signature, host manifest) | `https://api.calendar.aithos.world/.well-known/jwks.json` |
| Guarantor key set (entry manifests, attestations) | `https://api.calendar.aithos.world/trust-provider/.well-known/jwks.json` |
| Signed card and its key set | `https://api.calendar.aithos.world/agents/{id}/agent-card.json`, `.../agents/{id}/jwks.json` |
| A2A JSON-RPC endpoint (multi-tenant) | `https://api.calendar.aithos.world/a2a` |
| Scenario lab | `https://api.calendar.aithos.world/lab`, `.../lab/report?policy=guaranteed` |
| Live logs | `https://calendar.aithos.world/logs` (feed: `https://api.calendar.aithos.world/logs/events`) |
| Website | `https://calendar.aithos.world` |

## Verify it yourself

With `curl` and `jq`:

```sh
API=https://api.calendar.aithos.world
curl -si $API/.well-known/ai-catalog.json | sed -n '1,12p'          # media type, ETag, Link
curl -s $API/.well-known/ai-catalog.json | jq '.signature, .host.identifier, .entries[0].trustManifest'
curl -s $API/.well-known/jwks.json | jq .                          # operator keys (kid = RFC 7638 thumbprint)
curl -s $API/trust-provider/.well-known/jwks.json | jq .           # guarantor keys
CARD=$(curl -s $API/.well-known/ai-catalog.json | jq -r '.entries[0].url')
curl -s $CARD | sha256sum                                          # equals subject.digest
```

With the independent Python verifier (needs `pip install cryptography rfc8785`):

```sh
python3 scripts/verify-trust.py $API/.well-known/ai-catalog.json --policy guaranteed \
  --trusted-guarantor $API/trust-provider/.well-known/jwks.json
python3 scripts/verify-trust.py $API/lab/substituted-card/.well-known/ai-catalog.json   # FAIL card_digest_mismatch
```

With the AI Catalog tooling (`cargo install ai-catalog-cli`):

```sh
ai-catalog validate $API/.well-known/ai-catalog.json     # Level 3 (Trusted)
ai-catalog trust inspect $API/.well-known/ai-catalog.json
```

With the A2A CLI (`.build/tools/a2acli`, or the official `a2acli` release):
point it at any entry's card URL and send
`{"operation":"find_common_slot","peer":"urn:air:api.calendar.aithos.world:agent:<other id>","duration_minutes":30}`
to a mock agent; then open `https://calendar.aithos.world/logs?trace=<trace_id>`
from the reply to see each verification step.

The lab: `curl -s "$API/lab/report?policy=guaranteed" | jq .` runs every
scenario (tampered catalog, substituted card, replayed entry, expired
manifest, downgraded entry, unknown and impersonated guarantor, key
rotation, revoked-but-signed agent, mirror; plus unsigned, signed,
unguaranteed and forged callers) and reports the outcome against the
documented expectation. Each scenario catalog is a plain URL you can point
your own client at: `$API/lab/<scenario>/.well-known/ai-catalog.json`.
The scenarios are derived from the real published agents, so the lab
answers `409 {"error":"no_published_agent"}` on a deployment nobody has
signed in to yet (the caller cases need two agents).

## Run it locally

```sh
CALENDAR_LISTEN=127.0.0.1:3187 cargo run --locked       # fixture agents Alice and Bob, ephemeral keys
curl -s http://127.0.0.1:3187/lab | jq .
cargo test --locked                                    # unit, integration, lab and public-feed tests
cargo clippy --all-targets
```

Locally the operator and guarantor keys are in-memory and change on every
start; production keeps both in AWS KMS (`ECC_NIST_P256`) so the private
keys never leave the HSM.

## Configuration

| Variable | Meaning |
| --- | --- |
| `CALENDAR_PUBLIC_URL`, `CATALOG_URL`, `CALENDAR_WEBSITE_URL` | this deployment's API origin (also the `urn:air` publisher), the catalog it discovers peers in, the website |
| `TRUST_PROVIDER` | guarantor implementation; only `local` exists |
| `OPERATOR_KMS_KEY_ID`, `TRUST_KMS_KEY_ID` | KMS keys of the operator and the guarantor (unset: ephemeral in-memory keys) |
| `TRUSTED_GUARANTORS` | comma-separated guarantor identities (JWK Set URLs) the discovery client accepts; default: our own |
| `TRUST_POLICY_MOCK`, `TRUST_POLICY_LIVE`, `TRUST_POLICY_BOOKING` | `integrity`, `guaranteed` or `verified-account` per operation kind |
| `LAB_KEY_SEED` | derives the lab's successor and impostor keys identically on every instance |
| `PUBLIC_LOGS_TABLE`, `PUBLIC_LOGS_TTL_SECONDS` | public feed store (unset: memory) and retention (default 24 h) |

## Documentation

- [Trust layer reference](docs/trust-layer.md) — keys, signed bytes, verification algorithm, spec mapping, limits, replacing the guarantor
- [Trust manifest evaluation](docs/trust-manifest-evaluation.md) — field grid, lab results, limits, questions for the maintainers
- [SDK capabilities audit](docs/sdk-capabilities.md) — what the A2A and AI Catalog SDKs provide, what was implemented here and why
- [Logs and the public feed](docs/logging.md)
- [Operations and deployment](docs/operations.md), [Google OAuth setup](docs/google-oauth-setup.md)
- [Connected Google accounts: booking flow](docs/google-calendar-booking.md), [autonomous agent and inference budget](docs/autonomous-agent.md)
- [Archive](docs/archive/README.md) — earlier gates and the previous external-registry design

## Scope and guard-rails

Only this deployment's own catalog is served; nested catalogs and a root
catalog on an external registry are out of scope. Public page onboarding
requires no user authentication; a pasted page does not prove ownership.
Meeting bookings need a connected Google account on both sides, an
operation capability, and a signed request from a guaranteed caller. Keep
credentials, calendar samples, attendee details, Terraform state and
generated artifacts out of Git.
