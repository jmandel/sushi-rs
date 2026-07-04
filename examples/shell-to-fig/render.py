#!/usr/bin/env python3
"""Non-JS host: shell out to the `fig` CLI and consume its --json envelope.

Any language that can run a subprocess and parse JSON drives the engine this way
— no wasm, no FFI. Here we ask fig for its version and render one fragment from a
completed build tree, checking the shared apiVersion envelope exactly as the
wasm Session's callers do.

Usage: render.py <fig-binary> [<build-dir> <ref> <kind>]
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

    if len(sys.argv) >= 5:
        build, ref, kind = sys.argv[2], sys.argv[3], sys.argv[4]
        frag = fig_json(fig, "fragment", build, ref, kind)
        html = frag["html"]
        print(f"fragment {ref}-{kind}: {len(html)} bytes, starts with {html[:40]!r}")
    else:
        # Prove the error path is an ok:false envelope, not a crash.
        try:
            fig_json(fig, "snapshot")  # missing required arg
        except RuntimeError as e:
            print(f"error path is an envelope (not a crash): {e}")


if __name__ == "__main__":
    main()
