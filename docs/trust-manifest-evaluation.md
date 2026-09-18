# Evaluation of the AI Catalog trust manifest, from a working A2A deployment

Written on 2026-09-18 for the maintainers of [A2A](https://github.com/a2aproject/A2A)
and [AI Catalog](https://github.com/Agent-Card/ai-catalog), after implementing
Level 3 end to end in this repository (`docs/trust-layer.md`) and exercising
it with the scenario lab described below. Everything here can be reproduced
against the deployment or a local run.

## 1. Field-by-field grid

| Field | Question it answers | What it does **not** prove | Clarity of the specification |
| --- | --- | --- | --- |
| `identity` | Who signed this manifest, and where its verification key set lives (HTTPS form: fetch the JWK Set, select `kid`) | That the signer is trustworthy: any HTTPS URL on the publisher's domain is a valid identity. Trust is a client-side pin. | Clear on resolution per scheme. Unclear whether `identity` names the *artifact* ("primary subject identifier for this artifact") or the *signer* (key resolution reads it as the signer); this implementation uses the signer's JWK Set URL and documents it. |
| `identityType` | A hint for the scheme | — | Optional and free-form; omitted here because the scheme is evident. |
| `subject.digest` | The manifest speaks about **these exact bytes** | That the bytes are what the entry's `url` serves *now* (a live mirror or substitution is detected only by fetching and hashing) | Clear: digest format, SHA-256 minimum, "exact bytes served". |
| `subject.url` | The manifest was issued for **this** location | That other locations are illegitimate: a mirror needs its own manifest | Clear (MUST equal `entry.url`); the structural validator enforces it. |
| `subject.type` | The artifact's media type | — | Clear. |
| `signature` | Integrity and origin of the manifest, under the guarantor's key | Anything about the artifact's behaviour; anything after `expiresAt` | Clear (JCS, `signature` removed, detached compact JWS, algorithm allow-list, "MUST NOT let `alg` alone select"). |
| `issuedAt` / `expiresAt` | Validity window; a bound on the damage of a lost key or a revoked agent | Revocation: nothing invalidates a manifest before `expiresAt` | `expiresAt` is SHOULD-reject only; no guidance on typical lifetimes or clock skew. |
| `provenance[]` | Where the artifact came from (`publishedFrom`) | Anything verifiable: `relation` values are examples, `sourceId` is unchecked | Free semantics; useful as documentation, not as evidence. |
| `attestations[]` | Signed statements the guarantor attaches (here: `account-verified`) | Any shared meaning: `type` is free-form, so a consumer must know the guarantor's vocabulary | Structure is clear (`type`, `uri`, `digest`, `size`); semantics are entirely out of band. |
| `trustSchema` | Which framework the manifest follows | — | Not used here; would be the place to publish an attestation vocabulary. |
| `publisher` | Who lists the entry | That publisher and guarantor are distinct entities (both can be the operator) | Clear; alignment with `identity` domain is checked structurally. |
| `host.trustManifest` | The catalog operator's self-attestation (here: binds its own JWK Set) | Anything a third party vouched for | Awkward: a signed manifest needs a `subject`, and the host has no natural artifact; binding the operator JWK Set is this implementation's choice. |
| top-level `signature` | The catalog as a whole, including `host` and entry order, is what the operator published | That the operator is honest; that entries are current | Clear ("verified exactly as a Trust Manifest signature"), but only a SHOULD for Level 3. |

## 2. Lab results

The lab (`GET <api>/lab`, `src/lab.rs`) serves twelve catalogs derived
from the real records at request time, plus four inbound caller cases.
`GET <api>/lab/report?policy=<level>` runs this service's discovery client
against all of them and compares outcomes with the documented expectation;
every run is visible on `<site>/logs?trace=<trace_id>`. The Python verifier
(`scripts/verify-trust.py`) reproduces the same outcomes independently.

Results recorded on 2026-09-18 (local run; the production run is part of
CI):

| Scenario | `integrity` | `guaranteed` | `verified-account` (mock agents) |
| --- | --- | --- | --- |
| baseline | accepted | accepted | `attestation_missing` |
| tampered-catalog | `catalog_signature_invalid` | `catalog_signature_invalid` | `catalog_signature_invalid` |
| unsigned-catalog | accepted | `catalog_signature_missing` | `catalog_signature_missing` |
| substituted-card | `card_digest_mismatch` | `card_digest_mismatch` | `card_digest_mismatch` |
| replayed-entry | `manifest_subject_mismatch` | `manifest_subject_mismatch` | `manifest_subject_mismatch` |
| expired-manifest | accepted | `manifest_expired` | `manifest_expired` |
| downgraded (manifest removed) | `manifest_missing` | `trust_downgrade` | `trust_downgrade` |
| unknown-guarantor | accepted | `untrusted_guarantor` | `untrusted_guarantor` |
| impersonated-guarantor | accepted | `manifest_signature_invalid` | `manifest_signature_invalid` |
| rotated-key | accepted | accepted | `attestation_missing` |
| revoked-agent (spec limit) | accepted | accepted | `attestation_missing` |
| mirror | accepted | accepted | `attestation_missing` |
| caller: unsigned | accepted (mock data) | accepted (mock data) | accepted (mock data) |
| caller: signed | accepted | accepted | accepted |
| caller: unguaranteed issuer | `caller_unguaranteed` | `caller_unguaranteed` | `caller_unguaranteed` |
| caller: forged key | `caller_key_mismatch` | `caller_key_mismatch` | `caller_key_mismatch` |

With account-linked agents (production), `verified-account` accepts the
baseline, rotation and mirror scenarios, and the caller cases become
`caller_signature_missing` / capability check / `caller_unguaranteed` /
`caller_key_mismatch` (`tests/lab.rs`).

Observations that came out of running the lab:

- **Structural validation catches exactly one attack.** `ai-catalog-validate`
  rejects `replayed-entry` (`subject.url` ≠ `entry.url`) and reports every
  other scenario, including tampered, substituted and impersonated ones, as
  a valid **Trusted** catalog. Only cryptographic verification tells them
  apart.
- **A downgraded entry keeps Level 3.** Level detection considers only the
  manifests present; with the host manifest signed, an entry stripped of
  its manifest does not change the level (`tests/lab.rs`).
- **Key rotation works with no protocol support**, as long as the successor
  key is published in the JWK Set before it signs. The specification says
  nothing about overlap or retirement.
- **Revocation does not exist.** A revoked agent with a valid manifest is
  accepted until `expiresAt`.
- **Pinning is what makes guarantors mean something.** The Python verifier
  accepts `unknown-guarantor` when run without `--trusted-guarantor`, which
  is exactly what the specification allows: the impostor's identity is a
  well-formed HTTPS URL on the publisher's domain with a valid JWK Set.

## 3. Observed limits

1. **Self-signature when operator = guarantor.** The specification lets the
   operator be its own guarantor and gives a consumer no way to tell.
   Separating the keys (as done here) is cosmetic unless clients pin
   guarantors they trust for reasons outside the catalog.
2. **Redundancy between the card JWS and the manifest signature.** Both
   cover the card bytes. The manifest adds the guarantor, the window and
   attestations; the card JWS adds the agent's key. Consumers need guidance
   on which one to require when both exist.
3. **No revocation**, and no guidance on `expiresAt` lifetimes or clock skew.
4. **Key discovery and rotation.** `kid` selection from an HTTPS JWK Set is
   clear; there is no way to signal a compromised key, and `jku` (A2A) versus
   `identity` (AI Catalog) are two different mechanisms for the same need.
5. **`attestations` and `trustSchema` have no shared semantics.** The
   `account-verified` type used here is meaningful only to a client that
   already knows this guarantor's vocabulary.
6. **Identity/publisher domain alignment** blocks an external guarantor on a
   different domain: either the entries adopt the guarantor's domain as
   publisher, or the guarantor publishes a JWK Set under the publisher's
   domain. The rule protects against impersonation but conflates hosting,
   publishing and guaranteeing.
7. **Caller authentication is out of scope for both specifications.** The
   manifest protects the caller; nothing protects the called agent. The
   prototype in `src/trust/caller.rs` (a per-request JWS with the caller's
   published key, resolved through the catalog) closes the gap only
   between deployments that agree on the header.
8. **Cross-SDK card canonicalization.** The Python and JavaScript A2A signers
   drop empty values before JCS; the Rust SDK has no signer; a card signed
   as served may not verify in another SDK if it contains empty containers.
9. **Host manifest needs a subject.** A signed `host.trustManifest` must carry
   a `subject`, but the host has no artifact; binding its JWK Set is a
   workable convention the specification does not describe.

## 4. Questions and proposals for the maintainers

For **AI Catalog**:

- Say explicitly whether `identity` names the signer or the artifact, and,
  when it names the signer, recommend a media type for the JWK Set
  (`application/jwk-set+json`) and a `kid` policy (RFC 7638 thumbprints).
- Add a normative statement that Level 3 is only meaningful with
  **client-side pinning** of guarantors, and consider a `trustedGuarantors`
  concept in the consumer conformance section.
- Make level detection require a manifest on **every entry** whose trust is
  to be relied upon, or define a per-entry level, so a downgrade is visible.
- Define a minimal **revocation or freshness** mechanism: a maximum
  `expiresAt` lifetime, a `Cache-Control`-like hint, or a statement that
  consumers SHOULD re-fetch the catalog before each use.
- Define a **host manifest subject** convention (the operator's key set) or
  relax the `subject` requirement for host manifests.
- Publish an initial **attestation vocabulary** (`account-verified`,
  `publisher-identity`, …) or a way to declare one through `trustSchema`.
- Decide whether identity/publisher domain alignment should allow a
  **delegated guarantor** on another domain.
- Ship **cryptographic verification** in `ai-catalog-trust` (or a sibling
  crate); `src/trust/jose.rs` and `src/trust/verify.rs` here are a candidate
  contribution.

For **A2A**:

- Ship card **signing and verification in the Rust SDK**, and make the
  client factory able to refuse unverified cards; `src/trust/card.rs` is a
  candidate contribution.
- Specify the canonicalization of empty containers so signers in different
  languages agree on the payload.
- Consider an optional **caller authentication** profile (a signed request
  header bound to `messageId`, as prototyped here) or a pointer to how
  `securitySchemes` should be used between agents.

## 5. How to reproduce

```sh
# Local run with fixture agents and ephemeral keys
CALENDAR_LISTEN=127.0.0.1:3187 cargo run --locked
curl -s http://127.0.0.1:3187/lab | jq .
curl -s 'http://127.0.0.1:3187/lab/report?policy=guaranteed' | jq .
python3 scripts/verify-trust.py http://127.0.0.1:3187/lab/substituted-card/.well-known/ai-catalog.json
cargo test --locked --test lab --test public_logs
```

Against production, replace the origin with `https://api.calendar.aithos.world`
and add `--trusted-guarantor https://api.calendar.aithos.world/trust-provider/.well-known/jwks.json`
to the Python verifier. The live feed is at `https://calendar.aithos.world/logs`.
