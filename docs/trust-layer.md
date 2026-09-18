# Trust layer: what is signed, by whom, over which bytes, and how a client verifies it

This document is the end-to-end reference for the trust chain this proof of
concept implements on top of [A2A 1.0](https://a2a-protocol.org/latest/specification/)
and [AI Catalog](https://github.com/Agent-Card/ai-catalog/blob/main/specification/ai-catalog.md).
It is written for the maintainers of both specifications. Every claim here
is backed by code under `src/trust/`, by a test, or by a lab scenario an
external client can reproduce (`docs/trust-manifest-evaluation.md`).

## 1. What a trust manifest proves, and what it does not

- A `trustManifest` binds, unforgeably, **one catalog entry to one exact
  A2A Agent Card** (`subject.digest` over the served bytes), under the
  signature of an **identified guarantor**, for a **validity window**
  (`issuedAt`, `expiresAt`).
- If the card changes by one byte after it was guaranteed — by an attacker,
  the host, or the owner — a verifying client must refuse it. A legitimate
  change needs a new manifest.
- It proves **nothing about the agent's behaviour**. It protects the
  **caller** that discovers an agent. It does **not authenticate the
  caller** to the called agent (§7 covers that gap).
- Refusing is a **client-side policy**. Neither protocol enforces it: the
  Rust A2A SDK does not verify signatures, and the AI Catalog SDK does not
  verify cryptographically (`docs/sdk-capabilities.md`). This service
  applies a per-operation policy before any A2A call (§6).

## 2. Roles and keys

| Role | Key | Signs | Public key location |
| --- | --- | --- | --- |
| **Agent** (one P-256 key per agent, generated at onboarding) | in DynamoDB, attribute `signing_key`, never in API responses | its own A2A Agent Card; its outgoing A2A requests (§7) | `https://<api>/agents/{id}/jwks.json` — the card's `jku` |
| **Operator** (hosts catalog and cards) | AWS KMS `alias/calendar-production-operator`, `ECC_NIST_P256`, `SIGN_VERIFY`; the private key never leaves KMS | the catalog document (`signature`), the host `trustManifest` | `https://<api>/.well-known/jwks.json` — `host.identifier` and `publisher.identifier` |
| **Guarantor** (simulated trust provider) | AWS KMS `alias/calendar-production-guarantor`, same spec | every entry `trustManifest`, including its `attestations` | `https://<api>/trust-provider/.well-known/jwks.json` — every manifest's `identity` |

The operator and the guarantor are two keys even though one deployment runs
both, so a consumer pins guarantors independently of the catalogs it reads.
`TrustProvider` (`src/trust/mod.rs`) is the guarantor role; `LocalTrust` is
its only implementation. An external trust provider would be another
implementation selected by `TRUST_PROVIDER`, with the same identity/JWKS
contract, and neither the A2A code nor the catalog format would change.

Key identifiers are RFC 7638 thumbprints (`kid`), computed and checked by
`src/trust/jose.rs`; a served JWK Set whose `kid` is not the thumbprint of
its key is rejected, so a key cannot be aliased under a foreign identifier.

Locally (`CALENDAR_LISTEN`) and in tests, operator and guarantor keys are
ephemeral in-memory keys: the identities are still the same URLs, but the
keys change on every start. Production sets `OPERATOR_KMS_KEY_ID` and
`TRUST_KMS_KEY_ID`; startup fails closed if KMS cannot be reached.

## 3. The four signed artifacts, byte for byte

All canonicalization is RFC 8785 JCS through the `serde_jcs` crate, applied
to the raw JSON value (`src/trust/jose.rs::canonicalize`). All signatures
are ES256 (ECDSA P-256 with SHA-256, raw `r || s`), the only algorithm the
verifier accepts; `alg` is never used to select an algorithm.

### 3.1 A2A Agent Card (`src/trust/card.rs`)

- Payload: JCS(card **without** the top-level `signatures` member), after
  the card is normalized to the A2A 1.0 wire schema (`securityRequirements`
  as `schemes → StringList`).
- Signing input: `ASCII(BASE64URL(protected) || '.' || BASE64URL(payload))`
  (RFC 7515 §5.1, detached payload).
- Protected header: `{"alg":"ES256","kid":<agent kid>,"typ":"JOSE","jku":"https://<api>/agents/{id}/jwks.json"}` (`jku` only over HTTPS).
- Serialized form: `signatures: [{"protected": ..., "signature": ...}]` —
  exactly one signature, no unprotected `header`.
- **Served bytes** = JCS(card **with** `signatures`). The card is stored and
  served verbatim; `subject.digest` is `sha256:` over these exact bytes.

### 3.2 Entry `trustManifest` (`src/trust/manifest.rs`)

```json
{
  "identity": "https://<api>/trust-provider/.well-known/jwks.json",
  "subject": {"type": "application/a2a-agent-card+json",
              "digest": "sha256:<hex of the served card bytes>",
              "url": "https://<api>/agents/{id}/agent-card.json"},
  "issuedAt": "2026-09-18T16:13:29Z",
  "expiresAt": "2026-12-17T16:13:29Z",
  "provenance": [{"relation": "publishedFrom", "sourceId": "urn:air:<api-host>:agent:{id}"}],
  "attestations": [{"type": "account-verified", "uri": "data:application/json;base64,...",
                    "digest": "sha256:...", "size": 87,
                    "description": "The agent's account identity was verified by OpenID Connect sign-in."}],
  "signature": "<BASE64URL(protected)>..<BASE64URL(signature)>"
}
```

- Payload: JCS(manifest without `signature`), as the specification's
  §Signature Verification requires. `attestations` is present only for
  account-linked agents (Google sign-in with a verified e-mail).
- Signature: detached compact serialization `protected..signature`, protected
  header `{"alg":"ES256","kid":<guarantor kid>}`.
- Signed once at publication and stored with the record; re-signed (and
  persisted best-effort) when the served catalog finds it stale: wrong
  identity, wrong URL, wrong digest, or fewer than 7 days of validity left
  (`src/catalog.rs::current_manifest`). Validity is 90 days.

### 3.3 Host `trustManifest`

Same construction, signed by the **operator** key, with
`identity = host.identifier` and a subject that binds the operator JWK Set
itself: `{"type":"application/jwk-set+json","digest":sha256(JCS of the served JWKS),"url":"https://<api>/.well-known/jwks.json"}`.
The JWK Set is served in canonical form so the digest is reproducible.

### 3.4 Catalog document (`src/catalog.rs`)

- Payload: JCS(catalog without the top-level `signature`), entries sorted by
  agent id for determinism.
- Signature: detached compact, operator key, added as top-level `signature`.
- Served with `Content-Type: application/ai-catalog+json`, `ETag` = hex of
  the SHA-256 of the served bytes, `Cache-Control: public, max-age=60`, and
  `Link: <.../.well-known/ai-catalog.json>; rel="ai-catalog"`. The website
  sends the same `Link` header and declares `<link rel="ai-catalog">`.
- The signed document is cached per process while its unsigned content is
  unchanged, so the `ETag` is stable between changes. Size is bounded at
  1 MiB (about 600 account-linked entries); a larger catalog is refused rather than truncated.

Conformance: `ai-catalog-validate` reports **Trusted** (Level 3) and
`ai-catalog-trust::analyze_catalog` reports no error (`tests/identities.rs`,
`tests/lab.rs`). A unit test proves the SDK's canonicalization reproduces
these payloads byte for byte (`src/trust/manifest.rs::sdk_equivalence`).

## 4. Client-side verification, step by step (`src/discovery.rs`, `src/trust/verify.rs`)

Given a peer identifier `urn:air:<publisher>:agent:<id>` and a policy (§6):

1. **Catalog.** Fetch the configured catalog URL (HTTPS or loopback, no
   redirects, 3 s, ≤ 1 MiB). `specVersion` must be `1.x`. Resolve the
   operator key set from `host.identifier`, which must be an HTTPS JWK Set
   **on the catalog's origin** (`untrusted_operator` otherwise). If a
   `signature` is present it must verify (`catalog_signature_invalid`); if it
   is absent the `guaranteed` policy refuses (`catalog_signature_missing`)
   and `integrity` logs `catalog_unsigned`.
2. **Entry.** Entries with the identifier must be unique per
   (`identifier`, `version`) (`catalog_duplicate_entry`); the newest
   `updatedAt` wins; `type` must be `application/a2a-agent-card+json`.
3. **Manifest.** Absent: `trust_downgrade` under `guaranteed`,
   `manifest_missing` under `integrity`. Under `guaranteed`: `identity` must
   be a **pinned guarantor** (`untrusted_guarantor`), its JWK Set is fetched
   (cached 5 min), the detached JWS verifies (`manifest_signature_invalid`),
   `subject.type == entry.type` and `subject.url == entry.url`
   (`manifest_subject_mismatch`), `issuedAt ≤ now + 5 min`
   (`manifest_not_yet_valid`), `expiresAt > now` (`manifest_expired`), and
   `subject.digest` is `sha256:` + 64 lowercase hex (`manifest_malformed`).
   Under `integrity` only the subject binding is read; the signature is not
   checked (`manifest_unverified` is logged).
4. **Card bytes.** Fetch `entry.url` (network policy: agent origin only) and
   check `sha256(bytes) == subject.digest` through
   `ai_catalog_trust::verify_digest` (`card_digest_mismatch`).
5. **Card signature.** Exactly one `signatures[]` element, ES256, with a
   `kid`; `jku`, when present, must equal
   `https://<agent-origin>/agents/{id}/jwks.json` (`card_key_location_invalid`);
   that JWK Set is fetched and the JWS verified (`card_signature_invalid`).
6. **Attestation** (`verified-account` policy only): the verified manifest
   must carry an `account-verified` attestation (`attestation_missing`).
7. Only now is the card handed to the A2A SDK (`create_from_card`), with
   its JSON-RPC interfaces restricted to the agent origin and tenant.

Every step logs an event under target `calendar::trust`
(`catalog_fetched`, `catalog_signature_verified`, `manifest_verified`,
`card_digest_verified`, `card_signature_verified`, `peer_verified`, or the
matching `*_rejected` with `code`), with the trace id and the policy; the
public feed at `/logs` shows them (`docs/logging.md`).

`scripts/verify-trust.py` re-implements steps 1–6 in Python with the
`cryptography` package, so the chain can be checked without trusting this
code base; it reports the same codes.

## 5. Mapping to the specifications

| This implementation | A2A 1.0 | AI Catalog |
| --- | --- | --- |
| Card payload = JCS(card − `signatures`), `protected`/`signature` objects, `kid`, `jku`, `typ: JOSE` | §Agent Card Signing (JCS; `signatures` excluded; JWS protected header with `alg`, `kid`, optional `jku`; `typ` SHOULD be `JOSE`) | — |
| Cards at `/agents/{id}/agent-card.json` referenced by the catalog; no `/.well-known/agent-card.json` (multi-tenant) | §Agent Discovery (registries and curated catalogs) | entry `type: application/a2a-agent-card+json`, `url` |
| Manifest payload = JCS(manifest − `signature`), detached compact JWS, ES256 | — | §Trust Manifest, §Signature Verification, §Signature Algorithms |
| `identity` = HTTPS JWK Set URL; key selected by `kid` | — | §Key Resolution (HTTPS URL form); "consumers SHOULD pin the expected key via `kid`" |
| `urn:air:<api-host>:agent:<id>`; identity domain = publisher domain | — | §Identifiers (`urn:air` recommended), identity/publisher domain alignment (checked with `ai_catalog::identity_binds_to_entry`) |
| `subject {type, digest, url}` restating the entry | — | §Subject (`subject.url` MUST equal `entry.url`) |
| `expiresAt` honoured; `issuedAt` required with a signature | — | §Trust Manifest fields; "Consumers SHOULD reject a Trust Manifest whose `expiresAt` is in the past" |
| Top-level catalog `signature` | — | §Catalog-level integrity (Level 3 SHOULD) |
| `application/ai-catalog+json`, `/.well-known/ai-catalog.json`, `Link: rel="ai-catalog"`, `<link rel="ai-catalog">` | — | §Media type, §Well-known URI, §Link relation |
| HTTPS only, no redirects, bounded size and time, loopback only in tests | — | §Safe fetching |
| `attestations[]` with an inline `data:` URI | — | §Attestations ("consumers SHOULD prefer inline `data:` attestations") |

## 6. Per-operation policy (`src/trust/policy.rs`)

| Operation this service sends | Default level | Meaning |
| --- | --- | --- |
| Mock availability between demo agents | `integrity` | digest binding + card JWS; the manifest signature is not checked |
| Real availability (live booking pages, connected calendars) | `guaranteed` | + signed catalog, pinned guarantor, validity window, subject consistency |
| A booking that writes into a calendar | `verified-account` | + `account-verified` attestation |

`TRUST_POLICY_MOCK`, `TRUST_POLICY_LIVE` and `TRUST_POLICY_BOOKING` override
the defaults; `TRUSTED_GUARANTORS` is the comma-separated list of pinned
guarantor identities (default: this deployment's own). The policy is applied
before `create_from_card`; a refusal is a structured A2A reply with
`status: "error"` and the code, never a silent fallback.

## 7. Caller authentication (inbound), a prototype (`src/trust/caller.rs`)

Neither specification authenticates the calling agent. This service signs
each outgoing request with the caller's own agent key:

```
Agent-Signature: BASE64URL(protected) . BASE64URL(claims) . BASE64URL(signature)
protected = {"alg":"ES256","kid":<caller card kid>,"typ":"a2a-caller+jws"}
claims    = {"iss":<caller urn>,"aud":<callee urn>,"mid":<A2A messageId>,"op":<operation>,"iat":…,"exp":iat+60}
```

The called agent reads `iss`, resolves that identifier through the same
catalog chain (§4) at the policy level of the operation, requires `kid` to
be the key that signed the caller's card, verifies the JWS with the caller's
JWK Set, and checks `aud`, `mid`, `op` and the window. Real availability and
account-linked operations require the header (`caller_signature_missing`);
mock data verifies it only when present. Failures: `caller_unguaranteed`,
`caller_key_mismatch`, `caller_signature_invalid`, `caller_expired`,
`caller_audience_mismatch`, `caller_message_mismatch`. For account-linked
operations the signature's issuer must also be the caller named by the
operation capability (`caller_issuer_mismatch`). Limits: no replay cache
inside the 60 s window, no channel binding, and a compromised agent key is a
compromised caller.

## 8. Known limits

- **Self-guarantee.** The operator and the guarantor are two keys under one
  administration. The chain proves control of the deployment, not
  third-party vetting. Pinning is what makes the guarantor meaningful.
- **No revocation.** Neither specification defines it; a revoked agent with
  a still-valid manifest is accepted (lab scenario `revoked-agent`). Only
  `expiresAt` bounds the exposure.
- **Level detection ignores missing entry manifests.** `ai-catalog-validate`
  reports Trusted as long as every manifest *present* is signed, so an entry
  stripped of its manifest keeps the catalog at Level 3 (lab `downgraded`).
- **Redundancy.** The card JWS and the manifest signature both cover the
  card; the manifest adds the guarantor and the validity window, the card
  JWS adds the agent's own key. A client may reasonably require only one.
- **Cross-SDK card interoperability.** Python and JavaScript A2A signers strip
  empty values before canonicalizing; this service signs the card as served.
- **Per-agent private keys** are stored in DynamoDB (attribute
  `signing_key`) without envelope encryption; they are read only to sign
  outgoing requests. Wrapping them with KMS is a natural next step.
- **Origin allow-list.** The discovery client still only talks to its own
  agent origin; that is a network policy, not a trust decision, and the
  `mirror` scenario is served from the same origin for that reason.

## 9. Replacing `LocalTrust` with an external trust provider

An external provider implements `TrustProvider` (identity URI, JWK Set,
`manifest_for(entry, card_bytes, claims)`), is selected by `TRUST_PROVIDER`,
and publishes its JWK Set at its identity URL. Clients add that identity to
`TRUSTED_GUARANTORS`. Because the identity domain must equal the `urn:air`
publisher domain, an external guarantor either shares the publisher domain
or the entries adopt the guarantor's domain as publisher; this is a
constraint of the specification worth discussing (see the evaluation).
Nothing else changes: cards, catalog assembly, verification and logging are
provider-agnostic.
