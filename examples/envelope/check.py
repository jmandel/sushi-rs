#!/usr/bin/env python3
"""Validate fig --json envelopes against examples/envelope/schema.json.

Dependency-free (no jsonschema package): a minimal check of the documented shape
from both a success and a failure op. Usage: check.py <fig-binary>
"""
import json
import subprocess
import sys


def envelope(fig, *args):
    out = subprocess.run([fig, *args, "--json"], capture_output=True, text=True)
    return json.loads(out.stdout.strip().splitlines()[-1])


def check_success(env):
    assert set(env) == {"apiVersion", "ok", "op", "result"}, env.keys()
    assert env["apiVersion"] == 1 and env["ok"] is True and isinstance(env["op"], str)


def check_failure(env):
    assert set(env) == {"apiVersion", "ok", "op", "error"}, env.keys()
    assert env["apiVersion"] == 1 and env["ok"] is False
    assert set(env["error"]) == {"message"} and isinstance(env["error"]["message"], str)


def main():
    fig = sys.argv[1]
    check_success(envelope(fig, "version"))
    check_failure(envelope(fig, "snapshot"))  # missing required arg -> ok:false
    print("envelope schema: success + failure shapes OK")


if __name__ == "__main__":
    main()
