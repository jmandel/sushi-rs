# examples/ — runnable, CI-executed hosting examples

Every example here is run by [`scripts/examples-gate.sh`](../scripts/examples-gate.sh)
so the docs in [`docs/hosting.md`](../docs/hosting.md) and
[`README.md`](../README.md) can't rot. Run them all:

```sh
scripts/examples-gate.sh
#   FIG_BIN and F0_DIR override the fig binary and the F0 build root. Examples
#   needing an absent input SKIP with a note.
```

| Example | Skin | What it shows | Needs |
|---|---|---|---|
| `envelope/` | any | The shared apiVersion envelope schema + a validator | fig, python3 |
| `shell-to-fig/` | non-JS | Drive `fig --json` from Python; parse the envelope | fig, python3 |
| `cli-quickstart` (in the gate) | CLI | `fig render` a build tree → byte-checked vs the golden | fig, an F0 build |
| `template-as-data` (in the gate) | CLI | Same render path, different template — zero code | fig, an F0 build |

See `docs/hosting.md` for the prose. The external-builder example lives with its
real consumer in Cycle's `site-gen/README.md`: native `fig prepare`
emits the closed filesystem bundle and Cycle consumes it through
`SITE_BUILD_DIR`. The retired callback shim over a second WASM session is not a
supported example.
