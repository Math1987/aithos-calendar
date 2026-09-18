#!/usr/bin/env python3
"""Read-only production acceptance for the Calendar catalog and A2A flows."""
import datetime
import json
import sys
import urllib.error
import urllib.parse
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


def data(reply):
    return next(part["data"] for part in reply["result"]["message"]["parts"] if "data" in part)


def expected_slot(left, right, minutes):
    parse = lambda value: datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    candidates = []
    for a in left:
        for b in right:
            start = max(parse(a["start"]), parse(b["start"]))
            end = start + datetime.timedelta(minutes=minutes)
            if end <= min(parse(a["end"]), parse(b["end"])):
                candidates.append((start, end))
    return min(candidates) if candidates else None


def main(catalog_url):
    catalog = fetch(catalog_url)
    assert catalog["specVersion"] == "1.0"
    entries = catalog["entries"]
    assert len({e["identifier"] for e in entries}) == len(entries)
    origin = urllib.parse.urlsplit(catalog_url)
    api = f"{origin.scheme}://{origin.netloc}/a2a"
    urn = f"urn:air:{origin.hostname}:agent:"
    # Public onboarding must be reachable without IAM; invalid input creates nothing.
    try:
        fetch(f"{origin.scheme}://{origin.netloc}/agents", {"booking_page_url":"https://example.com/not-google"})
        raise AssertionError("Invalid booking URL was accepted")
    except urllib.error.HTTPError as error:
        assert error.code == 400, error.code
        assert json.load(error)["error"] == "invalid_booking_page_url"
    print("PASS anonymous onboarding reachable and invalid URL rejected")
    agents = []
    for entry in entries:
        assert entry["type"] == "application/a2a-agent-card+json"
        card = fetch(entry["url"])
        interface = next(i for i in card["supportedInterfaces"] if i["protocolBinding"] == "JSONRPC")
        assert interface["url"] == api
        tenant = interface["tenant"]
        assert entry["identifier"] == f"{urn}{tenant}"
        assert entry["trustManifest"]["subject"]["url"] == entry["url"]
        response = send(interface, tenant)
        assert response["result"]["message"]["parts"][0]["text"] == f"Hello from {card['name']}", response
        if card["version"] in ("0.5.0", "0.6.0", "0.6.1"):
            response = send(interface, tenant, {"operation":"get_availability"})
            # Account-linked agents refuse anonymous callers before the capability check.
            refused = ("error" in response and "result" not in response) or data(response)["code"] in (
                "caller_signature_missing", "a2a_authorization_required")
            assert refused, response
            print("PASS account-linked greeting; anonymous Calendar access rejected")
            continue
        availability = data(send(interface, tenant, {"operation": "get_availability"}))
        assert availability["agent"] == entry["identifier"]
        assert availability["mock"] is (card["version"] != "0.4.0"), availability
        if not availability["mock"]:
            metadata = fetch(f"{origin.scheme}://{origin.netloc}/agents/{tenant}/schedule")
            assert metadata["mock"] is False and 1 <= metadata["schedule"]["duration_minutes"] <= 1440
            assert "identity" not in availability and "email" not in availability
        agents.append((entry, interface, availability))
        print(f"PASS catalog → {card['name']} card → A2A greeting and availability")
    for tenant in [None, "unknown"]:
        response = send({"url": api}, tenant)
        assert response["error"]["code"] == -32602 and "result" not in response, response
        print(f"PASS rejected tenant {tenant!r}")
    if len(agents) < 2:
        print(f"PASS dynamic catalog ready ({len(agents)} agents); peer acceptance requires two published agents")
        return
    live = [a for a in agents if not a[2]["mock"]]
    mocks = [a for a in agents if a[2]["mock"]]
    pair = live[:2] if len(live) >= 2 else mocks[:2]
    if len(pair) < 2:
        print("PASS individual agents; pair acceptance requires two agents in the same mode")
        return
    for caller, peer in [(pair[0], pair[1]), (pair[1], pair[0])]:
        if not caller[2]["mock"]:
            result = data(send(caller[1], caller[1]["tenant"], {
                "operation":"find_common_slot", "peer":peer[0]["identifier"],
            }))
            assert result["mock"] is False and result["reserved"] is False, result
            assert result["status"] in ["slot_found", "no_common_slot"], result
            uuid.UUID(result["trace_id"])
            if result["status"] == "slot_found":
                parse = lambda v: datetime.datetime.fromisoformat(v.replace("Z", "+00:00"))
                assert parse(result["slot"]["end"]) - parse(result["slot"]["start"]) == datetime.timedelta(minutes=result["duration_minutes"])
            else:
                assert result["slot"] is None
            print(f"PASS live A2A exchange: {result['status']}; trace_id={result['trace_id']}")
            continue
        for duration in [30, 60]:
            result = data(send(caller[1], caller[1]["tenant"], {
                "operation": "find_common_slot", "peer": peer[0]["identifier"], "duration_minutes": duration,
            }))
            expected = expected_slot(caller[2]["slots"], peer[2]["slots"], duration)
            assert result["status"] == ("slot_found" if expected else "no_common_slot"), result
            assert result["mock"] is True and result["reserved"] is False, result
            uuid.UUID(result["trace_id"])
            if expected:
                assert result["slot"] == {key: date.isoformat().replace("+00:00", "Z") for key, date in zip(["start", "end"], expected)}, result
            else:
                assert result["slot"] is None, result
            print(f"PASS {caller[0]['displayName']} → {peer[0]['displayName']}: {result['status']}; trace_id={result['trace_id']}")
    caller = agents[0]
    result = data(send(caller[1], caller[1]["tenant"], {
        "operation": "find_common_slot", "peer": f"{urn}missing", "duration_minutes": 30,
    }))
    assert result["status"] == "error" and result["code"] == "peer_not_found", result
    print("PASS absent peer rejected")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python3 scripts/smoke-a2a.py TRUSTED_CATALOG_URL")
    main(sys.argv[1])
