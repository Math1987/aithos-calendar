#!/usr/bin/env python3
"""Run a command with local .env credentials; never source shell code or print values."""
import os
from pathlib import Path
import shlex
import sys

root = Path(__file__).resolve().parents[1]
env = os.environ.copy()
allowed = {"AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN",
           "AWS_DEFAULT_REGION", "AWS_REGION", "GH_TOKEN", "ANAKIN_API_KEY",
           "BOOKING_TEST_FIRST_NAME", "BOOKING_TEST_LAST_NAME", "BOOKING_TEST_EMAIL"}
path = root / ".env"
if path.exists():
    for line in path.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, sep, value = line.partition("=")
        if not sep or key not in allowed:
            raise SystemExit("Unsupported .env entry; use the names in .env.example")
        parts = shlex.split(value, comments=True)
        if len(parts) != 1:
            raise SystemExit("Each .env entry must contain exactly one value")
        env[key] = parts[0]
    if env.get("AWS_ACCESS_KEY_ID"):
        env.pop("AWS_PROFILE", None)
        env.pop("AWS_DEFAULT_PROFILE", None)
env["AWS_PAGER"] = ""
if len(sys.argv) < 2:
    raise SystemExit("Usage: python3 scripts/with-env.py COMMAND [ARG ...]")
os.execvpe(sys.argv[1], sys.argv[1:], env)
