#!/usr/bin/env python3
"""Independent verifier for the trust chain of a deployment.

Reimplements, in Python, exactly what src/trust/verify.rs checks, so an
auditor can confirm the Rust side without trusting it:

  1. catalog signature  : detached JWS (ES256, kid) over JCS(catalog - signature),
                          key set fetched from host.identifier (operator JWKS)
  2. entry manifests    : detached JWS over JCS(manifest - signature), key set
                          fetched from trustManifest.identity (guarantor JWKS);
                          subject.type/url restate the entry; validity window
  3. card digest        : sha256(exact card bytes) == subject.digest
  4. card signature     : A2A signatures[0] {protected, signature} over
                          JCS(card - signatures), key set at <api>/agents/<id>/jwks.json
                          (must equal the protected header's jku when present)

Usage: python3 scripts/verify-trust.py CATALOG_URL [--policy integrity|guaranteed|verified-account]
                                       [--trusted-guarantor JWKS_URL ...]

Requires the `cryptography` package. Canonicalization uses the `rfc8785`
package when installed and otherwise json.dumps(sort_keys) which is
byte-identical for these documents (no floats, no unusual escapes).
"""
import argparse
import base64
import datetime
import hashlib
import json
import sys
import urllib.parse
import urllib.request

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.hazmat.primitives.asymmetric.utils import encode_dss_signature

try:
    import rfc8785

    def canonicalize(value):
        return rfc8785.dumps(value)
except ImportError:  # pragma: no cover - fallback path
    def canonicalize(value):
        return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def b64url(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def b64url_decode(text):
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def fetch(url):
    with urllib.request.urlopen(url, timeout=20) as response:
        return response.read(), response.headers


def keys_from(jwks):
    keys = {}
    for jwk in jwks["keys"]:
        if jwk.get("kty") != "EC" or jwk.get("crv") != "P-256":
            continue
        thumbprint = b64url(hashlib.sha256(canonicalize({k: jwk[k] for k in ("crv", "kty", "x", "y")})).digest())
        assert jwk["kid"] == thumbprint, f"kid {jwk['kid']} is not the RFC 7638 thumbprint"
        x = int.from_bytes(b64url_decode(jwk["x"]), "big")
        y = int.from_bytes(b64url_decode(jwk["y"]), "big")
        keys[jwk["kid"]] = ec.EllipticCurvePublicNumbers(x, y, ec.SECP256R1()).public_key()
    return keys


def verify_jws(protected_b64, payload, signature_b64, keys):
    header = json.loads(b64url_decode(protected_b64))
    assert header["alg"] == "ES256", f"unexpected alg {header['alg']}"
    key = keys[header["kid"]]
    raw = b64url_decode(signature_b64)
    assert len(raw) == 64
    der = encode_dss_signature(int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big"))
    key.verify(der, protected_b64.encode() + b"." + b64url(payload).encode(), ec.ECDSA(hashes.SHA256()))
    return header


def verify_detached(document, keys, what):
    protected, payload, signature = document["signature"].split(".")
    assert payload == "", f"{what}: detached JWS must have an empty payload segment"
    stripped = {k: v for k, v in document.items() if k != "signature"}
    try:
        return verify_jws(protected, canonicalize(stripped), signature, keys)
    except (InvalidSignature, KeyError) as error:
        raise AssertionError(f"{what}_signature_invalid ({error.__class__.__name__})") from None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("catalog_url")
    parser.add_argument("--policy", default="guaranteed", choices=["integrity", "guaranteed", "verified-account"])
    parser.add_argument("--trusted-guarantor", action="append", default=[],
                        help="pin a guarantor identity (JWKS URL); repeatable. Without it, any identity "
                             "resolvable per the specification is accepted, which is what the spec allows "
                             "and what a real client must not do.")
    args = parser.parse_args()
    level = ["integrity", "guaranteed", "verified-account"].index(args.policy)
    origin = urllib.parse.urlsplit(args.catalog_url)
    api = f"{origin.scheme}://{origin.netloc}"

    raw, headers = fetch(args.catalog_url)
    assert headers.get("Content-Type", "").startswith("application/ai-catalog+json"), headers.get("Content-Type")
    catalog = json.loads(raw)
    assert catalog["specVersion"].split(".")[0] == "1"
    operator = catalog["host"]["identifier"]
    assert urllib.parse.urlsplit(operator).netloc == origin.netloc, "untrusted_operator: key set must be on the catalog origin"
    operator_keys = keys_from(json.loads(fetch(operator)[0]))
    if "signature" in catalog:
        header = verify_detached(catalog, operator_keys, "catalog")
        print(f"PASS catalog signature (kid {header['kid']})")
    else:
        assert level < 1, "catalog_signature_missing"
        print("WARN catalog is unsigned (tolerated by the integrity policy)")
    host_manifest = catalog["host"].get("trustManifest")
    if host_manifest:
        verify_detached(host_manifest, operator_keys, "host_manifest")
        jwks_bytes = fetch(operator)[0]
        assert host_manifest["subject"]["digest"] == "sha256:" + hashlib.sha256(jwks_bytes).hexdigest()
        print("PASS host manifest binds the operator JWK Set")

    now = datetime.datetime.now(datetime.timezone.utc)
    guarantors = {}
    for entry in catalog["entries"]:
        name = entry["identifier"]
        assert entry["type"] == "application/a2a-agent-card+json"
        manifest = entry.get("trustManifest")
        assert manifest is not None, f"{name}: trust_downgrade" if level >= 1 else f"{name}: manifest_missing"
        subject = manifest["subject"]
        assert subject["type"] == entry["type"] and subject["url"] == entry["url"], f"{name}: manifest_subject_mismatch"
        if level >= 1:
            identity = manifest["identity"]
            assert urllib.parse.urlsplit(identity).hostname == name.split(":")[2], f"{name}: identity domain must equal the urn:air publisher"
            if args.trusted_guarantor:
                assert identity in args.trusted_guarantor, f"{name}: untrusted_guarantor ({identity})"
            elif identity not in guarantors:
                print(f"WARN accepting guarantor {identity} without pinning (spec key resolution only)")
            if identity not in guarantors:
                guarantors[identity] = keys_from(json.loads(fetch(identity)[0]))
            verify_detached(manifest, guarantors[identity], f"{name}: manifest")
            issued = datetime.datetime.fromisoformat(manifest["issuedAt"].replace("Z", "+00:00"))
            assert issued <= now + datetime.timedelta(minutes=5), f"{name}: manifest_not_yet_valid"
            if "expiresAt" in manifest:
                expires = datetime.datetime.fromisoformat(manifest["expiresAt"].replace("Z", "+00:00"))
                assert expires > now, f"{name}: manifest_expired"
        card_bytes, _ = fetch(entry["url"])
        assert subject["digest"] == "sha256:" + hashlib.sha256(card_bytes).hexdigest(), f"{name}: card_digest_mismatch"
        card = json.loads(card_bytes)
        tenant = name.rsplit(":", 1)[1]
        jwks_url = f"{api}/agents/{tenant}/jwks.json"
        [signature] = card["signatures"]
        header = json.loads(b64url_decode(signature["protected"]))
        assert header.get("jku", jwks_url) == jwks_url, f"{name}: card_key_location_invalid"
        agent_keys = keys_from(json.loads(fetch(jwks_url)[0]))
        stripped = {k: v for k, v in card.items() if k != "signatures"}
        try:
            verify_jws(signature["protected"], canonicalize(stripped), signature["signature"], agent_keys)
        except (InvalidSignature, KeyError):
            raise AssertionError(f"{name}: card_signature_invalid") from None
        if level >= 2:
            kinds = {a.get("type") for a in manifest.get("attestations", [])}
            assert "account-verified" in kinds, f"{name}: attestation_missing"
        print(f"PASS {name}: manifest, digest and card signature ({args.policy})")
    print(f"PASS {len(catalog['entries'])} entries verified under policy {args.policy}")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as error:
        print(f"FAIL {error}")
        sys.exit(1)
