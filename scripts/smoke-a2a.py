#!/usr/bin/env python3
"""Acceptance check for trusted mock catalog URLs; no credentials or booking writes."""
import json
import sys
import urllib.request
import uuid


def fetch(url, payload=None):
    data = None if payload is None else json.dumps(payload).encode()
    request = urllib.request.Request(url, data=data, headers={
        "Content-Type": "application/json", "A2A-Version": "1.0",
    })
    with urllib.request.urlopen(request, timeout=15) as response:
        return json.load(response)


def send(interface, tenant):
    return fetch(interface["url"], {
        "jsonrpc": "2.0", "id": str(uuid.uuid4()), "method": "SendMessage",
        "params": {
            **({"tenant": tenant} if tenant is not None else {}),
            "message": {"messageId": str(uuid.uuid4()), "role": "ROLE_USER",
                        "parts": [{"text": "Hello"}]},
        },
    })


def main(catalog_url):
    catalog = fetch(catalog_url)
    assert catalog["specVersion"] == "1.0"
    entries = {entry["identifier"]: entry for entry in catalog["entries"]}
    for tenant, name in [("alice", "Alice"), ("bob", "Bob")]:
        entry = entries[f"urn:aithos:calendar:agent:{tenant}"]
        assert entry["type"] == "application/a2a-agent-card+json"
        card = fetch(entry["url"])
        assert card["name"] == name
        interface = next(i for i in card["supportedInterfaces"] if i["protocolBinding"] == "JSONRPC")
        assert interface["tenant"] == tenant
        response = send(interface, interface["tenant"])
        assert response["result"]["message"]["parts"][0]["text"] == f"Hello from {name}", response
        print(f"PASS catalog → {name} card → A2A greeting")
    for tenant in [None, "unknown"]:
        response = send(interface, tenant)
        assert response["error"]["code"] == -32602, response
        assert "result" not in response
        print(f"PASS rejected tenant {tenant!r}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python3 scripts/smoke-a2a.py TRUSTED_CATALOG_URL")
    main(sys.argv[1])
