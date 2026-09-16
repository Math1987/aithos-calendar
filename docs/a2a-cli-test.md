# Alice/Bob A2A acceptance gate

Status: deployed and verified locally and in production on 2026-09-16.
This is the historical gate-2 acceptance record, following the Rust foundation.
For the subsequent Alice → Bob exchange, see [gate 3](agent-collaboration.md).

## What is real and what is mocked

Discovery, Agent Cards, A2A 1.0 JSON-RPC serialization, tenant routing and HTTP
requests are real. Only application behavior is mocked: Alice and Bob return
`Hello from Alice` and `Hello from Bob`. No calendars, bookings, LLM, user accounts,
caller authentication or memory are involved. Fixtures contain no private data.

The same Lambda serves:

| Route | Purpose |
| --- | --- |
| `GET /health` | Existing liveness check |
| `GET /.well-known/ai-catalog.json` | URL catalog listing both mock agents |
| `GET /agents/{tenant}/agent-card.json` | One public card per known agent |
| `POST /a2a` | Shared A2A JSON-RPC endpoint |

The official `a2a-server-lf` 0.4.4 router handles the protocol; `a2a-lf` 0.3.1
provides protocol types. `ai-catalog` 0.2.1 provides catalog types. There is no
fork or registry server. The small stateless `RequestHandler` returns an immediate
Message; it deliberately avoids the SDK's in-memory task store, which would not
provide durable state across Lambda instances. Streaming, task operations, push
notifications and extended cards are unsupported. Stateful continuations and
non-text messages are rejected. No background work outlives the response.

Agent configuration is defined once in `src/agents.rs` and drives the catalog,
cards and response selection. `src/a2a.rs` implements mock behavior; `src/lib.rs`
assembles HTTP routes; `src/main.rs` selects Lambda or local HTTP execution.
`CALENDAR_PUBLIC_URL` supplies the canonical API origin; request Host headers
never determine advertised URLs. Existing AWS resources and IAM roles are reused,
with three new API routes and three route-scoped Lambda invoke permissions.

## Registry Aithos later

`CATALOG_URL` is the **discovery entry point**. For this gate it points to Calendar.
Later it can point to an AI Catalog exposed by registry Aithos, containing the
same Agent Card URLs. The cards continue to advertise Calendar's `/a2a` endpoint
and the appropriate tenant. The registry does not become the destination for
A2A messages just because discovery starts there.

The commands below assume a direct AI Catalog 1.0 JSON document with URL entries.
Do not substitute an arbitrary registry home/search URL. If Aithos exposes a
different API or nested catalogs, adapt the discovery step when integrating it.
No registry publication has been performed in this gate.

## CLI prerequisites

Use the official Rust CLI `a2acli` **0.2.1**, plus `curl` and `jq`.
For this workstation a release binary with a checked SHA-256 is already available:

```sh
cd "/Volumes/Math17/aithos/R&D/calendar"
export PATH="$PWD/.build/tools:$PATH"
a2acli --version
```

On another machine, install the pinned published release:

```sh
cargo install a2a-cli --version 0.2.1 --locked
```

Upstream: [official CLI](https://github.com/a2aproject/a2a-rs/tree/main/a2acli).
The CLI automatically sends the tenant advertised by the selected card interface.
Do not use `--tenant` to override a card's declared tenant; use direct endpoint
mode for the negative test below.

## Manual production test

### 1. Load the catalog

```sh
export CATALOG_URL="https://api.calendar.aithos.world/.well-known/ai-catalog.json"
curl -fsS "$CATALOG_URL" | jq .
```

Expect URL entries for Alice and Bob. Select their card URLs from those entries:

```sh
ALICE_CARD=$(curl -fsS "$CATALOG_URL" | jq -er '.entries[] | select(.identifier == "urn:aithos:calendar:agent:alice") | .url')
BOB_CARD=$(curl -fsS "$CATALOG_URL" | jq -er '.entries[] | select(.identifier == "urn:aithos:calendar:agent:bob") | .url')
```

### 2. Inspect cards, then call both agents

```sh
a2acli --agent-card "$ALICE_CARD" -o json card get
a2acli --agent-card "$BOB_CARD" -o json card get
a2acli --agent-card "$ALICE_CARD" send "Hello"
a2acli --agent-card "$BOB_CARD" send "Hello"
```

Both cards advertise the same `/a2a` URL and JSONRPC 1.0. Their tenants differ.
Expected replies: `Hello from Alice`, then `Hello from Bob` (exit 0).
For the protocol response use `-o json` before `send`.

### 3. Reject an unknown tenant

Derive the endpoint from Bob's card, then bypass card resolution for this test:

```sh
A2A_ENDPOINT=$(curl -fsS "$BOB_CARD" | jq -er '.supportedInterfaces[] | select(.protocolBinding == "JSONRPC") | .url')
a2acli --endpoint "$A2A_ENDPOINT" --transport jsonrpc --tenant unknown send "Hello"
echo $?
```

Expect `INVALID_PARAMS`, `Unknown or missing tenant`, A2A code `-32602`, and exit 1.
Omitting `--tenant unknown` also fails: the shared endpoint has no default agent.
An unknown Agent Card URL returns HTTP 404. JSON-RPC errors themselves use HTTP
200 with an error envelope; use the CLI exit status or JSON error, not HTTP alone.

These are CLI → Alice and CLI → Bob calls. Alice → Bob is the next gate.

## Local checks and CI

```sh
cargo fmt --check
cargo test --locked
CALENDAR_LISTEN=127.0.0.1:3000 cargo run --locked
```

In another terminal set `CATALOG_URL` to
`http://127.0.0.1:3000/.well-known/ai-catalog.json` and repeat the same commands.
Stop the local server with Ctrl-C. Optional `CALENDAR_PUBLIC_URL` overrides the
local advertised origin when a proxy is involved.

The CI runs the integration tests before building the Linux Lambda and, after
apply, checks health plus catalog → cards → greetings and tenant rejection:

```sh
python3 scripts/smoke-a2a.py "$CATALOG_URL"
```

This smoke script expects the two mock identifiers and follows supplied URLs;
use a trusted test catalog. It uses the wire protocol directly. The separate
manual acceptance above uses the official A2A client implementation.

## Verification record

- Four Rust integration tests passed locally, covering discovery, tenant errors,
  unsupported stateful/streaming methods and malformed/non-text messages.
- Official CLI 0.2.1 returned both greetings from card-derived tenants locally.
- CLI rejected unknown and missing tenants with exit 1 and unknown cards with exit 3.
- The actual executable passed local Lambda Runtime API + HTTP API v2 checks for
  health, catalog, Agent Card and A2A message routes.
- Terraform validation passed.
- Deployed application commit: `eb8b772875faa595b4f36252159a496a08929376`.
- [Production workflow](https://github.com/Math1987/aithos-calendar/actions/runs/35059585542)
  succeeded: 4 integration tests, Linux release build, health check and catalog/A2A smoke checks.
- Terraform applied 6 additions (3 routes and 3 invoke permissions), 1 in-place
  Lambda update and 0 deletions. Existing API, domains and website were retained.
- Official CLI 0.2.1 also passed the complete catalog → card → message sequence
  against the production domain for both agents, with card-derived tenants.
  Missing and unknown tenants returned `INVALID_PARAMS` / `-32602`, exit 1.
- HTTPS health and blank website remain functional. CloudWatch invocation reports
  showed successful execution of the new binary, with no observed runtime errors.
- The local test server was stopped after acceptance; the release CLI remains
  available under ignored `.build/tools/` for the owner's manual checks.
