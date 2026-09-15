"""The stage-one liveness endpoint has no external dependencies."""
import json


def handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json", "cache-control": "no-store"},
        "body": json.dumps({"status": "ok", "service": "calendar"}, separators=(",", ":")),
    }
