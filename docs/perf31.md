# 31-IG Two-Phase Performance Harness

`harness/perf31.sh` measures the self-reliant build path in two explicit phases:

1. **materialize**: a populated CAS plus an IG lock file becomes a `.fhir/packages`
   style materialized cache. Benchmark mode runs this with `--offline` by default,
   so the timed path is local hardlink/copy work plus derived-index installation
   only.
2. **build**: `rust_sushi build <ig> --cache <materialized-cache>` compiles from an
   already materialized package tree.

The harness defaults to `temp/perf31` and `temp/perf31/cas`; it never defaults to
the real `~/.fhir`.

Latest median-of-3 baseline on the 31-IG corpus after the allocator pass
(`mimalloc` global allocator in the CLI, run `20260701-093651`):

```text
materialize median-sum: 0.595s
build median-sum:       17.184s
total:                  17.779s
```

The previous no-allocator score was `0.866s` materialize + `23.863s` build =
`24.729s` total. The pre-optimization one-pass baseline was `64.052s`
materialize + `37.708s` build = `101.760s` total. Materialization is now a small
part of the 31-IG total; the remaining build tails are compiler/model work.

## Common Runs

```sh
cargo build --release -q

# Prepare locks and populate the CAS. This is setup, not part of the timing.
OFFLINE=0 harness/perf31.sh prepare

# Time all 31 IGs, three iterations each. Each iteration starts with no
# materialized cache for that IG, then builds from the cache just created.
harness/perf31.sh bench
```

Useful knobs:

```sh
RUNS=5 harness/perf31.sh bench ips mcode safr
PERF31_WORK=/tmp/perf31 FHIR_CAS=/tmp/perf31/cas OFFLINE=0 harness/perf31.sh prepare
RUST_SUSHI_VERIFY_CAS=1 harness/perf31.sh bench ips
harness/perf31.sh summarize temp/perf31/runs/<stamp>/results.csv
```

## Profiling

Build with symbols and frame pointers when collecting samples:

```sh
CARGO_PROFILE_RELEASE_DEBUG=1 RUSTFLAGS="-C force-frame-pointers=yes" cargo build --release -q
OFFLINE=0 harness/perf31.sh prepare mcode
harness/perf31.sh profile build mcode
harness/perf31.sh profile materialize mcode
```

The profile mode writes `perf.data`, `perf-report.txt`, logs, and the exact command
under `temp/perf31/profile/<phase>-<ig>-<stamp>/`.

## Interpreting Results

`bench` writes `results.csv` with one row per IG, iteration, and phase:

```text
ig,iter,phase,status,seconds,packages,files,bytes,src,cache,out,log
```

`summary.txt` reports median and best observed time for each phase. The
`TOTAL median-sum` line is the best quick whole-corpus scorecard: it is the sum of
per-IG medians, split between materialization and compiler build time.

Materialization trusts CAS entries by default. CAS packages are validated and
made read-only at ingest; set `RUST_SUSHI_VERIFY_CAS=1` when debugging suspected
cache corruption and you want the older per-materialize manifest check.
