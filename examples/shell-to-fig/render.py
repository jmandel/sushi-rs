#!/usr/bin/env python3
"""Non-JS host: shell out to the `fig` CLI and consume its --json envelope.

Any language that can run a subprocess and parse JSON drives the engine this way
— no wasm, no FFI. Here we ask Fig for its version and exercise the shared
success/error envelope used by WASM Session callers.

Usage: render.py <fig-binary>
"""
import json
import subprocess
import sys


def fig_json(fig, *args):
    """Run `fig <args> --json` and return the parsed envelope, raising on ok:false."""
    out = subprocess.run([fig, *args, "--json"], capture_output=True, text=True)
    line = out.stdout.strip().splitlines()[-1] if out.stdout.strip() else ""
    env = json.loads(line)
    # The ONE envelope contract (schema-identical to the wasm Session):
    #   { apiVersion, ok, op, result | error:{message} }
    assert env["apiVersion"] == 1, f"unsupported apiVersion {env['apiVersion']}"
    if not env["ok"]:
        raise RuntimeError(f"{env['op']}: {env['error']['message']}")
    return env["result"]


def main():
    fig = sys.argv[1]
    ver = fig_json(fig, "version")
    print(f"engine: {ver['engine']} (apiVersion {ver['apiVersion']})")

    # Prove the error path is an ok:false envelope, not a crash.
    try:
        fig_json(fig, "snapshot")  # missing required arg
    except RuntimeError as e:
        print(f"error path is an envelope (not a crash): {e}")


if __name__ == "__main__":
    main()
