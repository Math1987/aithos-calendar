# Application and A2A SDK logs

Status: deployed and verified in production on September 16, 2026.

## Format and sources

The Rust process initializes one `tracing-subscriber` JSON formatter at startup.
Both application and SDK events go to stderr, which Lambda collects in CloudWatch.
Each line is one JSON object with `timestamp`, `level` and `target`.

| `target` | Source | Typical contents |
| --- | --- | --- |
| `calendar::a2a` | Calendar application | `operation_received`, `negotiation_completed`, outcome status |
| `calendar::identities` / `calendar::registry` / `calendar::storage` | Calendar identity lifecycle | Creation, publication and provider failures |
| `calendar::discovery` | Calendar application | `peer_call`, caller, peer and recipient tenant |
| `a2a_client::middleware` | A2A SDK client | `A2A client request`, `A2A client response`, `A2A client error` |
| `a2a_server::middleware` | A2A SDK server | `A2A server request`, `A2A server response`, `A2A server error` |

Application events use `event`; SDK events use `message` and `method`. Protocol
errors are `WARN`. A valid A2A reply containing a business outcome such as
`peer_not_found` is still a successful protocol response; inspect the application's
`status` and `code` to understand that outcome.

Each handled A2A request has a span (request context) with `span.tenant` and
`span.trace_id`. For SendMessage, a valid UUID supplied in metadata
`calendarTraceId` is propagated; otherwise a new UUID is generated. The coordinating agent passes
it to the recipient, and response data still contains `trace_id` at the same place.
The context is attached to the async future, so concurrent requests do not share
or overwrite their tenant/trace fields. Startup/runtime events may lack this context.

**Log schema change:** application logs previously had `tenant` and `trace_id` at
the top level. They now share `span.tenant` and `span.trace_id` with SDK logs. The
existing plain-text trace-ID search still works. Historical CloudWatch events
keep their previous format.

Example shape (illustrative, not a recorded event):

```json
{"timestamp":"2030-01-15T09:00:00Z","level":"INFO","message":"A2A client request","method":"SendMessage","target":"a2a_client::middleware","span":{"name":"a2a_request","tenant":"alice","trace_id":"00000000-0000-4000-8000-000000000001"}}
```

## SDK integration

- Client 0.2.5: register its native `LoggingInterceptor` on the SDK client factory.
- Server 0.4.4: the SDK declares `LoggingInterceptor` and `InterceptedHandler`, but
  does not implement `RequestHandler` for the wrapper. Our small `server_call`
  adapter calls the SDK's native before/after logging hooks around each Calendar
  handler method. The SDK itself emits these events, preserving its own target.
  No fork or patch of the SDK is used.
- The server adapter supplies status-only placeholders to the logging hooks;
  request/response bodies and headers are not serialized for logging. It is
  specific to `LoggingInterceptor`, not a general authentication interceptor.

Scope: valid requests that reach our typed A2A handler, including greetings,
unknown tenants and unsupported methods. Malformed JSON and dispatch failures
rejected earlier by the SDK's router do not pass through this adapter. Streaming
is unsupported; if implemented later, stream consumption needs its own tracing.
The adapter logs request handling, not proof that response bytes reached the client.

No bodies, calendar intervals or authorization headers are added to these logs.
SDK errors include their error description. AWS invocation reports remain separate.
A tenant or trace ID is a correlation value, not proof of caller identity.

## Levels

Default filter:

```text
warn,calendar=info,a2a_client=info,a2a_server=info
```

`RUST_LOG` overrides it. It must be a valid tracing filter; invalid configuration
fails at startup. To enable SDK diagnostics locally while retaining request context:

```sh
RUST_LOG=warn,calendar=info,a2a_client=debug,a2a_server=debug \
  CALENDAR_LISTEN=127.0.0.1:3187 cargo run --locked
```

Keep the `calendar=info` directive to retain request spans, even when viewing only
SDK events. Production uses the built-in default; any future Lambda environment
change should go through Terraform. No infrastructure or IAM change is needed.

## Read CloudWatch

From the repository directory, using the existing ignored `.env` credentials:

```sh
python3 scripts/with-env.py aws logs tail \
  /aws/lambda/calendar-production-health --region eu-west-3 \
  --since 10m --format short --filter-pattern '"trace_id"' --follow
```

In another terminal, run the host → guest CLI request from the
[dynamic identities guide](dynamic-agents.md). Stop the log tail with Ctrl+C.

For **SDK events only**, replace the filter argument with:

```sh
--filter-pattern '{ $.target = "a2a_client::middleware" || $.target = "a2a_server::middleware" }'
```

For **application events only**:

```sh
--filter-pattern '{ $.target = "calendar::a2a" || $.target = "calendar::discovery" || $.target = "calendar::identities" || $.target = "calendar::registry" || $.target = "calendar::storage" }'
```

For **one exchange**, replace it with the returned trace ID in double quotes:

```sh
--filter-pattern '"YOUR-TRACE-ID"'
```

CloudWatch Logs Insights query for the new format:

```text
fields @timestamp, level, target, span.tenant, message, event, status, code, span.trace_id
| filter ispresent(span.trace_id)
| sort @timestamp asc
| limit 200
```

## Verification

`tests/logging.rs` launches the real binary, performs Alice → Bob and Bob → Alice
concurrently and checks the emitted JSON. It verifies application/client/server
sources, matching trace IDs, correct caller/recipient tenants, a server warning
for an unknown tenant, and that sentinel body/header values are absent. The
logging gate ran 15 tests. Gate 4 adds three identity tests, for 18 in total.

Production acceptance on **September 16, 2026**:

- Application commit: `85fd8745ad9ab633bcee2530d34bf0d53c7a399b`.
- [GitHub Actions run](https://github.com/Math1987/aithos-calendar/actions/runs/35066357537): all 15 tests passed, Linux binary built, deployment and production A2A smoke checks passed.
- Terraform: 0 resources added, 1 changed (Lambda), 0 destroyed.
- Official CLI selected Alice's card from the catalog and received `slot_found`, with `reserved: false`.
- Trace `01a0a908-b961-759f-a485-4cb39fb1ad24` appeared in two Lambda log streams, with 10 correlated events: 4 Calendar events, 2 native SDK client events and 4 native SDK server events.
- Both documented CloudWatch source filters were executed successfully: SDK returned the exchange's 6 SDK events; Calendar returned its 4 application events.
