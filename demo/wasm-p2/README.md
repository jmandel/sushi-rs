# WASM P2 — production JS surface + Web Worker demo

The `wasm_api` crate (wasm-bindgen) driving the rust_sushi compiler + the
snapshot walk engine on `wasm32-unknown-unknown`, exercised by a plain page + Web
Worker (no framework). This records the early production-shaped spike: a
typed `init`/`compile`/`generate_snapshot` surface over an in-memory
`BundleSource` package cache — the WASI shim of P0 is gone.

The current production host contract is documented in `docs/hosting.md`.

## What it does

1. **init** — fetch the cycle IG's package bundles (`.tgz`), inflate + mount them
   into a `BundleSource` (the browser's package cache).
2. **compile** — run the compiler in-memory over the cycle IG's `sushi-config.yaml`
   + `input/fsh/**` + `input/resources/**`, returning the compiled FHIR resources.
3. **generate_snapshot** — per profile, generate a validation-grade snapshot.

All three run in the Worker; the UI thread never blocks. Timings are shown per
step.

## Prerequisites

- A **`wasm32-unknown-unknown`** toolchain (upstream rustup matching the repo
  rustc; `rustup target add wasm32-unknown-unknown`). Its std ships prebuilt —
  simpler than P0's wasip1.
- **wasm-bindgen** CLI matching the crate's `wasm-bindgen` version
  (`cargo install wasm-bindgen-cli --version <ver>`).
- A populated package cache at `temp/fhir-home/.fhir/packages` and the cycle IG
  (default: the P0 demo copy at `demo/wasm-p0/data/vfs/cycle`; run
  `demo/wasm-p0/prepare.sh` first, or set `CYCLE_DIR`).

The prepare script honors `WASM_RUSTUP_HOME` / `WASM_CARGO_HOME` / `WASM_BINDGEN`
to point at a scratch toolchain without touching your default rustup.

## Run it

```sh
# 1. Build the wasm module (web target) + cycle bundles + IG sources into data/.
WASM_BINDGEN=/path/to/wasm-bindgen demo/wasm-p2/prepare.sh

# 2. Serve + open.
cd demo/wasm-p2 && python3 -m http.server 8020   # -> http://localhost:8020/
```

## Headless timings (no browser)

The same init → compile → snapshot flow under Node (nodejs-target module), for
CI-capturable timings:

```sh
node demo/wasm-p2/drive-node.mjs \
  "$PWD/<wasm-nodejs-dir>" "$PWD/demo/wasm-p2/data/bundles" "$PWD/demo/wasm-p2/data/cycle"
```

## Parity gate

Byte parity native↔wasm (the ladder + ips/mcode/sdc corpus against the wasm
build) is proven by `bash scripts/wasm-parity.sh` — see that script and §9c.

`data/` is gitignored (regenerable, large).
