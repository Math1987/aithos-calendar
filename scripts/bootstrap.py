#!/usr/bin/env python3
"""Plan/apply bootstrap and migrate its state once. Run with operator AWS credentials."""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]
INFRA = ROOT / "infra" / "bootstrap"
BUCKET = "aithos-calendar-tfstate-128066560720-eu-west-3"
KEY = "bootstrap/terraform.tfstate"


def run(*args):
    subprocess.run(args, cwd=ROOT, check=True)


def aws_exists(*args):
    result = subprocess.run(["aws", *args, "--no-cli-pager"], capture_output=True, text=True)
    if result.returncode == 0:
        return True
    if any(code in result.stderr for code in ["(404)", "(NoSuchKey)", "(NoSuchBucket)"]):
        return False
    raise SystemExit("Cannot inspect bootstrap backend. Check AWS identity/permissions; no changes made.")


def remote_backend():
    return f'''terraform {{
  backend "s3" {{
    bucket = "{BUCKET}"
    key = "{KEY}"
    region = "eu-west-3"
    encrypt = true
    use_lockfile = true
  }}
}}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["plan", "apply"])
    args = parser.parse_args()
    identity = json.loads(subprocess.check_output(
        ["aws", "sts", "get-caller-identity", "--output", "json", "--no-cli-pager"]))
    if identity["Account"] != "128066560720":
        raise SystemExit("Wrong AWS account; expected aithos-prod")
    bucket_exists = aws_exists("s3api", "head-bucket", "--bucket", BUCKET)
    remote_exists = bucket_exists and aws_exists("s3api", "head-object", "--bucket", BUCKET, "--key", KEY)
    local_state = INFRA / "terraform.tfstate"
    backend = INFRA / "backend.generated.tf"
    tf = ["terraform", f"-chdir={INFRA}"]
    if remote_exists:
        if local_state.exists() and not backend.exists():
            raise SystemExit("Both remote and local state exist; reconcile before initializing")
        backend.write_text(remote_backend())
        run(*tf, "init", "-input=false")
    else:
        if backend.exists():
            raise SystemExit("Configured remote bootstrap state is missing; recover state before continuing")
        if bucket_exists and not local_state.exists():
            raise SystemExit("Bucket exists without known state; inspect/import instead of recreating")
        run(*tf, "init", "-input=false")

    plan = INFRA / "bootstrap.tfplan"
    if args.action == "plan":
        run(*tf, "plan", "-input=false", "-lock-timeout=5m", f"-out={plan}")
        return
    if not plan.exists():
        raise SystemExit("Run and review bootstrap.py plan before apply")
    run(*tf, "apply", "-input=false", str(plan))
    if not remote_exists:
        if aws_exists("s3api", "head-object", "--bucket", BUCKET, "--key", KEY):
            raise SystemExit("Unexpected remote state appeared; refusing to overwrite it")
        backend.write_text(remote_backend())
        run(*tf, "init", "-migrate-state", "-force-copy", "-input=false")
        if not aws_exists("s3api", "head-object", "--bucket", BUCKET, "--key", KEY):
            raise SystemExit("Migration not verified; retain local recovery state")
        print("Bootstrap state migrated and verified. Local recovery files remain ignored.")
    run(*tf, "state", "list")


if __name__ == "__main__":
    main()
