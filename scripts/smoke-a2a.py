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
    with urllib.request.urlopen(request, timeout=25) as response:
        return json.load(response)


def send(interface, tenant, operation=None):
    return fetch(interface["url"], {
        "jsonrpc": "2.0", "id": str(uuid.uuid4()), "method": "SendMessage",
        "params": {
            **({"tenant": tenant} if tenant is not None else {}),
            "message": {"messageId": str(uuid.uuid4()), "role": "ROLE_USER",
                        "parts": [{"data": operation}] if operation is not None else [{"text": "Hello"}]},
        },
    })


def main(catalog_url):
    catalog = fetch(catalog_url)
    assert catalog["specVersion"] == "1.0"
    entries = {entry["identifier"]: entry for entry in catalog["entries"]}
    interfaces = {}
    for tenant, name in [("alice", "Alice"), ("bob", "Bob")]:
        entry = entries[f"urn:aithos:calendar:agent:{tenant}"]
        assert entry["type"] == "application/a2a-agent-card+json"
        card = fetch(entry["url"])
        assert card["name"] == name
        interface = next(i for i in card["supportedInterfaces"] if i["protocolBinding"] == "JSONRPC")
        assert interface["tenant"] == tenant
        interfaces[tenant] = interface
        response = send(interface, interface["tenant"])
        assert response["result"]["message"]["parts"][0]["text"] == f"Hello from {name}", response
        print(f"PASS catalog → {name} card → A2A greeting")
    for tenant in [None, "unknown"]:
        response = send(interface, tenant)
        assert response["error"]["code"] == -32602, response
        assert "result" not in response
        print(f"PASS rejected tenant {tenant!r}")


    for caller, peer, duration, expected in [
        ("alice", "bob", 30, "slot_found"),
        ("bob", "alice", 30, "slot_found"),
        ("alice", "bob", 60, "no_common_slot"),
        ("alice", "missing", 30, "error"),
    ]:
        reply = send(interfaces[caller], caller, {
            "operation": "find_common_slot",
            "peer": f"urn:aithos:calendar:agent:{peer}",
            "duration_minutes": duration,
        })
        result = next(part["data"] for part in reply["result"]["message"]["parts"] if "data" in part)
        assert result["status"] == expected, result
        assert result["mock"] is True and result["reserved"] is False, result
        uuid.UUID(result["trace_id"])
        if expected == "slot_found":
            assert result["slot"] == {"start": "2030-01-15T09:30:00Z", "end": "2030-01-15T10:00:00Z"}, result
        elif expected == "no_common_slot":
            assert result["slot"] is None, result
        else:
            assert result["code"] == "peer_not_found", result
        print(f"PASS {caller} → {peer}: {expected}; trace_id={result['trace_id']}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python3 scripts/smoke-a2a.py TRUSTED_CATALOG_URL")
    main(sys.argv[1])
