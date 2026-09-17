#!/usr/bin/env python3
"""Run a command with local credentials; never source shell code or print values."""
import json
import os
from pathlib import Path
import shlex
import sys

# Legacy test attendee values remain accepted, but booking no longer uses them.
ALLOWED = {"AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN",
           "AWS_DEFAULT_REGION", "AWS_REGION", "GH_TOKEN", "ANAKIN_API_KEY",
           "BOOKING_TEST_FIRST_NAME", "BOOKING_TEST_LAST_NAME", "BOOKING_TEST_EMAIL"}
AWS_JSON = {"AccessKeyId": "AWS_ACCESS_KEY_ID", "SecretAccessKey": "AWS_SECRET_ACCESS_KEY",
            "SessionToken": "AWS_SESSION_TOKEN"}


def parse_credentials(text):
    """Accept dotenv entries and a pasted AWS credential-process JSON object."""
    values = {}
    while text.strip():
        text = text.lstrip()
        if text.startswith("{"):
            try:
                obj, end = json.JSONDecoder().raw_decode(text)
            except ValueError:
                raise ValueError("Invalid AWS credential JSON") from None
            if set(obj) - (set(AWS_JSON) | {"Version", "Expiration"}) or not all(
                isinstance(obj.get(key), str) and obj[key].strip() for key in AWS_JSON
            ):
                raise ValueError("Expected AWS JSON with AccessKeyId, SecretAccessKey and SessionToken")
            incoming = {name: obj[key] for key, name in AWS_JSON.items()}
            text = text[end:]
        else:
            line, _, text = text.partition("\n")
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            key, sep, value = line.strip().partition("=")
            if not sep or key not in ALLOWED:
                raise ValueError("Unsupported .env entry; use the names in .env.example")
            try:
                parts = shlex.split(value, comments=True)
            except ValueError:
                raise ValueError("Invalid quoting in .env entry") from None
            if len(parts) != 1:
                raise ValueError("Each .env entry must contain exactly one value")
            incoming = {key: parts[0]}
        if values.keys() & incoming.keys():
            raise ValueError("Duplicate credential fields; keep only one AWS credential set")
        values.update(incoming)
    return values


def main():
    env = os.environ.copy()
    path = Path(__file__).resolve().parents[1] / ".env"
    if path.exists():
        try:
            env.update(parse_credentials(path.read_text()))
        except ValueError as error:
            raise SystemExit(str(error)) from None
        if env.get("AWS_ACCESS_KEY_ID"):
            env.pop("AWS_PROFILE", None)
            env.pop("AWS_DEFAULT_PROFILE", None)
    env["AWS_PAGER"] = ""
    if len(sys.argv) < 2:
        raise SystemExit("Usage: python3 scripts/with-env.py COMMAND [ARG ...]")
    os.execvpe(sys.argv[1], sys.argv[1:], env)


if __name__ == "__main__":
    main()
