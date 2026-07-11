# WASM P0 — rust_sushi + snapshot_gen in the browser

A throwaway-quality early spike
proving the Rust toolchain — **rust_sushi** (FSH → FHIR resources) and
**snapshot_gen** (the walk snapshot engine) — runs unmodified in a browser via
WASI, compiling the cycle IG and generating validation-grade snapshots whose
output is **byte-identical to native**.

This is a spike: prove-don't-polish. The production shape (a `PackageSource`
trait + OPFS cache + a `wasm_api` bindgen crate) is P1/P2, not here.

## What it does

The page (`index.html` + `app.js`) runs two plain WASI programs against an
in-memory virtual filesystem, exactly mirroring the native CLI:

```
rust_sushi   build /work/cycle -o /work/out --cache /work/packages
snapshot_gen --cache /work/packages --package hl7.fhir.r5.core#5.0.0 \
             --local-dir /work/out/fsh-generated/resources <profile.json>
```

It reports build + per-profile snapshot timings and one profile's element count,
and exports `wasm-hashes.json` (SHA-256 of every output) for the byte-match gate.

## Code changes to make this build (the only ones)

- `crates/rust_sushi/Cargo.toml` + `src/main.rs`: **mimalloc is now cfg-gated to
  non-wasm targets**. mimalloc is a C dependency with no wasm libc (`wchar.h`
  missing); wasm falls back to the default allocator. Native behavior is
  unchanged — same global allocator on every non-wasm target.
- `snapshot_gen` needed **zero** changes. No `Instant::now`/clock shims were
  needed (`wasm32-wasip1` provides wall-clock via WASI; the codebase's only
  time calls are in `#[cfg(test)]` or `package_acquisition`, which the WASI
  binaries don't exercise at compile time).

## Prerequisites

- A **wasm32-wasip1** Rust toolchain. Arch's system rustc cannot cross-compile
  std for it and its target-std is ABI-incompatible with upstream prebuilt std,
  so use a rustup-managed **upstream** toolchain matching the repo's rustc:
  ```sh
  export RUSTUP_HOME=/path/to/scratch/rustup CARGO_HOME=/path/to/scratch/cargo
  curl -sSf https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init | sh -s -- \
    -y --no-modify-path --profile minimal --default-toolchain 1.96.0 -t wasm32-wasip1
  export PATH="$CARGO_HOME/bin:$PATH"
  ```
  (Match `1.96.0` to `rustc --version`. On a normal rustup box:
  `rustup target add wasm32-wasip1`.)
- A populated FHIR package cache at `temp/fhir-home/.fhir/packages` (already in
  the repo). The cycle IG's closure: `hl7.fhir.r4.core#4.0.1`,
  `hl7.fhir.uv.tools.r4#1.1.2`, `hl7.terminology.r4#7.2.0`,
  `hl7.fhir.uv.extensions.r4#5.3.0` (build) + `hl7.fhir.r5.core#5.0.0` (snapshot).
- `strace` (for the minimal-cache trace; optional — falls back to full package
  dirs if absent).

## Run it

```sh
# 1. Build wasm binaries + assemble the (gitignored) demo data.
demo/wasm-p0/prepare.sh

# 2. Serve locally.
cd demo/wasm-p0 && python3 -m http.server 8000

# 3. Open http://localhost:8000/ and click "Run compile + snapshot".
#    Append ?auto=1 to auto-run; ?strace=1 or ?debug=1 for WASI-syscall tracing.
```

## Byte-match gate (browser output == native output)

```sh
# In the page: click Run, then "download wasm-hashes.json".
# Then:
demo/wasm-p0/check-native.sh ~/Downloads/wasm-hashes.json
#   -> BYTE-MATCH GATE: PASS   (18 entries: 11 build resources + 7 snapshots)
```

`check-native.sh` runs the same inputs natively and diffs the SHA-256 manifests.

## How the minimal cache is chosen

The package store reads each package's `.index.json` + `.derived-index.json`
eagerly (metadata) and then reads resource **bodies lazily**, only for the
names/URLs it fishes. `prepare.sh` traces a real native build+snapshot under
`strace` and ships exactly that read set (plus the index metadata) — ~8 MB
instead of the ~230 MB full closure. The read set is deterministic (same code +
inputs ⇒ same fished files), so it is complete; the byte-match gate is the proof.

## Gotcha we hit (documented so P1 doesn't rediscover it)

Package directory names contain `#` (e.g. `hl7.fhir.r4.core#4.0.1`). A raw `#`
in a URL is a fragment delimiter, so `fetch("data/vfs/packages/…#4.0.1/…")`
silently drops everything after `#` and 404s. `app.js` therefore
`encodeURIComponent`s every path segment. (Under Node/wasmtime this never
surfaces because they read the disk directly.)

## Verified with

- **Chromium 148 headless** (real browser, CDP-driven): compile+snapshot in-tab,
  byte-match gate PASS.
- **wasmtime 46** and **Node's `node:wasi`**: same binaries + virtual-FS layout,
  byte-identical to native — cross-runtime confirmation the parity is in the
  code, not one shim.
