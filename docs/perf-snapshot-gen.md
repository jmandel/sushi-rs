# Perf log — snapshot_gen walk engine (task #12a PERF)

Protocol: measure → hypothesize → change → re-measure → gate. Parity is sacred;
any ok-count change or byte diff = revert. One change per cycle. Release build,
best-of-5 warm per-IG batch wall (like docs/perf-protocol.md).

Machine: 12 cores, Linux. Cache: `temp/fhir-home/.fhir/packages`. Gate:
`snapshot/check-harvested-r4.sh` (debug binary batch → `diff-snapshot.cjs`).
Bench: release `snapshot_gen --batch-list` (generate-only, isolates engine),
package lists = the AGENTS.md gate context per IG.

## Baseline (best-of-5 warm) — commit 8b9fc84 (CUTOVER COMPLETE)

| IG | profiles | batch wall (best-of-5) |
|---|--:|--:|
| davinci-pas | 80 | 11.861 s |
| sdc | 73 | 0.860 s |
| us-core | 70 | 2.032 s |
| qicore | 63 | 2.813 s |
| gematik-epa-medication | 49 | 0.915 s |
| **full-corpus sweep (27 IGs that ran)** | — | 120.8 s |

Fixture ladder (`cargo test -p snapshot_gen`, walk_parity + convert_parity +
units): timed below.

## Profile (perf -F1500, davinci-pas batch, before any change)

Top self-time symbols:
- `serde_json ... Value::deserialize`  ~21.6%
- `serde_json SliceRead::parse_str`    ~13.2% (+ `from_utf8` ~8.7%)
- libc malloc/free family              ~11%+
- **Every walk function** (`process_paths`, `updatefromdef`, `sliced`, `simple`,
  `finalize`, merge) is **< 0.1% self-time each.** The walk itself is not the cost.

### Root cause (measured, not assumed)
A **1-profile** davinci-pas batch takes **11.6 s** — i.e. essentially the whole
batch time is the one-time `PackageContext` load, not per-profile walk work.

`PackageContext::load_package` calls `probe_name(path)` for **every**
StructureDefinition file in **every** loaded package, and `probe_name` does a
full `serde_json::from_slice::<Value>` of the file purely to read the top-level
`"name"` string. davinci-pas loads **5,860 SD files = 220.7 MB of JSON**, all
fully parsed at load. That parse is the ~21%+13%+8% serde cost above and scales
with SD-file count/size across every IG:

| IG | SD files loaded | 1-profile batch (≈ load time) |
|---|--:|--:|
| davinci-pas | 5860 | 11.6 s |
| us-core | 5467 | 1.76 s |
| qicore | 1972 | 2.62 s |
| sdc | 3437 | 0.66 s |
| gematik | 2170 | 0.36 s |

Cross-item memoization levers from the task brief were **measured and rejected as
non-costs**: instrumented `resolve_with_snapshot` shows only 480 resolve calls
for the whole davinci-pas batch (6/profile) and **0 recursive gen-misses** (local
bases ship snapshots in the fixtures dir); the shared `fetch_cache` already
avoids re-reads. So a per-batch `gen_cache` would help nothing here.

`by_name` is only the **last-resort fallback** in `resource_path` (after by_url,
by_id, both from `.index.json` with no file parse). Instrumented across the full
corpus: **0 by_name fallback hits in any IG.** The entire `probe_name` parse
builds an index that is never consulted.

### Hypothesis
Kill / shrink the eager full-parse in `probe_name`. Option chosen: byte-scan the
top-level `"name"` instead of full-parsing (proven sushi-side pattern,
perf-map.md area A), producing a byte-identical `by_name` index. If residual I/O
still dominates, make `by_name` fully lazy.

## Change 1 — byte-scan `probe_name` (ACCEPTED)

- **Hypothesis:** the load-time `by_name` index is built by full-parsing every SD
  file just to read `"name"`; a depth-aware byte-scan for the top-level `"name"`
  gives a byte-identical index for far less work.
- **Change:** `package.rs` `probe_name` now calls `scan_top_level_name(&bytes)`
  (depth-aware scan of the root object's own `"name"` property, value decoded via
  serde for exact escape handling) and only full-parses as a fall-through when the
  scan can't cleanly extract a top-level string. +unit tests
  (`name_scan_tests::synthetic_shapes`) + a corpus-equivalence test that checks
  scan==parse over every cached SD file.
- **Parity evidence:** `corpus_equivalence` **checked 19,742 SD files, 0
  mismatches.** Gates (debug binary, real `diff-snapshot.cjs`):
  davinci-pas 80/80, sdc 73/73, us-core 70/70, qicore 63/63, gematik 49/49,
  ips 29/29, mcode 46/46. `cargo test -p snapshot_gen` all green.
- **Measurement (best-of-5 warm):**

  | IG | before | after change 1 | Δ |
  |---|--:|--:|--:|
  | davinci-pas | 11.861 | 11.098 | −6% |
  | sdc | 0.860 | 0.353 | −59% |
  | us-core | 2.032 | 1.240 | −39% |
  | qicore | 2.813 | 2.580 | −8% |
  | gematik | 0.915 | 0.645 | −30% |

- **Verdict:** ACCEPTED. Big win for IGs whose load is dominated by
  index-listed SDs. davinci-pas barely moved → its load cost is elsewhere
  (next).

## Profile 2 — davinci-pas residual is the scan-fallback, not probe_name

Instrumented per-package load time: a **0-profile** davinci-pas batch (pure
`PackageContext::new`) is **10.7 s**, and it is concentrated in a handful of
packages with `loaded=0`:

| package | load | json files | SD files |
|---|--:|--:|--:|
| us.nlm.vsac#0.18.0 | 3721 ms | 15,333 | 0 |
| us.nlm.vsac#0.19.0 | 3595 ms | 15,333 | 0 |
| us.nlm.vsac#0.11.0 | 1508 ms | ~9k | 0 |
| us.nlm.vsac#0.7.0 | 1071 ms | ~6k | 0 |
| us.cdc.phinvads#0.12.0 | 821 ms | 1,968 | 0 |

These ValueSet/CodeSystem packages have **zero** StructureDefinitions in their
`.index.json`, so `load_package` falls into `scan_package_structure_definitions`,
which **full-parses every JSON file** (line ~142) only to check
`resourceType == "StructureDefinition"` and find none. vsac 0.18.0 alone is 1.2 GB
of ValueSet JSON, all parsed to discover 0 SDs.

### Hypothesis 2
Byte-check `resourceType` before full-parsing in the scan fallback: only parse a
file that is actually a StructureDefinition. Behaviour is unchanged (the scan
already keeps only SD files); it just skips the wasted parse of every non-SD file.

## Change 2 — byte-check `resourceType` in scan fallback (ACCEPTED)

- **Change:** generalized `scan_top_level_name` → `scan_top_level_string(bytes,
  key)`; `scan_package_structure_definitions` now byte-scans `resourceType`
  first and `continue`s on a definite non-SD (`Some(other)`), only full-parsing
  files that are (or might be, `None`→fallback) StructureDefinitions. Exactly
  equivalent to the prior `from_slice(...).resourceType == "StructureDefinition"`
  filter; it just skips parsing the 15k ValueSet files that were never SDs.
- **Parity evidence:** `name_scan_tests` green (incl. corpus equivalence, 19,742
  files, 0 mismatches). Gates: davinci-pas 80/80, sdc 73/73, us-core 70/70,
  qicore 63/63, gematik 49/49, **subscriptions-backport 9/9** (the IG that
  depends on the scan-fallback full-conversion path), pas 73/73, ips 29/29,
  mcode 46/46. `cargo test -p snapshot_gen` all green.
- **Measurement (best-of-5 warm):**

  | IG | baseline | after ch.1 | after ch.2 | total Δ |
  |---|--:|--:|--:|--:|
  | davinci-pas | 11.861 | 11.098 | **1.370** | **−88%** |
  | sdc | 0.860 | 0.353 | 0.355 | −59% |
  | us-core | 2.032 | 1.240 | **0.533** | **−74%** |
  | qicore | 2.813 | 2.580 | **0.562** | **−80%** |
  | gematik | 0.915 | 0.645 | 0.649 | −29% |

- **Verdict:** ACCEPTED. Both changes are one file (`package.rs`), byte-identical
  output, provably equivalent extractors with a corpus equivalence test.

## Post-optimization profile — walk engine is NOT a cost; load path dominates

Full 80-profile davinci-pas batch (perf -F2500), flat self-time:
- `path::compare_components` + `quicksort` ~21% — `files.sort()` over the
  ~15k-entry vsac/phinvads directory listings + PathBuf HashMap keys (load path).
- `scan_top_level_string` 7.9% + `fs::read`/`File::open` ~7.6% (load path).
- residual serde `deserialize`/`parse_str`/`skip_to_escape` ~13% (base resolve +
  index parse; load path).
- **Entire walk engine ≈ 0.6% total self-time:** `process_paths` 0.14%,
  `Comparer::find` 0.14%, `finalize` 0.05%, everything else < 0.05% each.

**Conclusion:** the walk itself has no hotspot. The task-brief walk-internal
levers (id→index side maps, `find_end_of_element`/`get_diff_matches` linear
scans, accidental Value clones) target functions that do not register on the
profile — optimizing them would add complexity for zero measurable gain and the
CLARITY pass would (rightly) reject it. **No walk-internal change made:
measurement does not justify one.**

### Load-path successor (coordinator direction, Josh) — designed, NOT done here
The remaining load cost (directory scan+sort, per-process `probe_name`/
`resourceType` scanning, base re-reads) has a strategic fix that lands in the
CLARITY pass, out of PERF scope: extend the CAS generated-index format
(`package_acquisition`) with **derived columns (`name` at minimum)** computed once
at materialization and consumed by both `package_store` and snapshot_gen's
`PackageContext`, deleting per-process probing entirely and unifying the three
parallel package-reading layers. Changes 1 & 2 are the interim win; they are
strictly subsumed by that fix (same data, computed once vs per-process), so no
further load-path micro-optimization (e.g. avoiding the `files.sort()` quicksort,
lazy `by_name`, caching directory scans) was pursued — **logged as
rejected-superseded** against the CAS-column successor.

## Final numbers (best-of-5 warm, release, both changes in)

| IG | profiles | baseline | final | Δ |
|---|--:|--:|--:|--:|
| davinci-pas | 80 | 11.861 s | **1.370 s** | −88% |
| sdc | 73 | 0.860 s | 0.355 s | −59% |
| us-core | 70 | 2.032 s | 0.533 s | −74% |
| qicore | 63 | 2.813 s | 0.562 s | −80% |
| gematik | 49 | 0.915 s | 0.649 s | −29% |

Full-corpus **gate** sweep (`check-harvested-r4.sh`, debug binary +
`diff-snapshot.cjs`, authoritative AGENTS.md package lists), 30 IGs:
**every IG at its §9 scorecard count** (ips 29, mcode 46, crd 22, sdc 73, dtr 21,
ecr 28, ndh 50, pas 73, carinbb 6, mhd 42, eu-eps 23, eu-mpd 4, au-ps 17,
pacio-toc 4, dapl 26, us-core 70, ipa 12, qicore 63, au-core 26, genomics 33
[with pinned extensions.r4#5.3.0], cdex 8, plan-net 22, pdex 37, drug-formulary 19,
subscriptions-backport 9, smart-app-launch 6, davinci-pas 80, gematik 49,
radiation-dose-summary 4, pddi 1) — **failed=0 everywhere.** Sweep wall ≈ 92 s
(debug + node diff; the release generate-only equivalent is far lower).

Any scorecard mismatch seen while sweeping (genomics 32/33 under a package-deps
extensions version, twpas 20/43 under a package-deps Twcore closure) was
**reproduced byte-identically by the pre-change binary via `git stash`** — i.e. a
wrong package *context* in the sweep script, never a regression. With the correct
documented context both are at scorecard (genomics 33/33, and twpas is covered by
its own gate list).

`cargo test -p snapshot_gen` all green (units + `walk_parity` 17-rung ladder 0.51 s
+ `convert_parity` + the new `name_scan_tests` incl. 19,742-file corpus
equivalence). Ladder test wall: ~0.5 s warm (excluding the one-time
corpus_equivalence scan).

## SCOPE B — sushi compiler drift check (confirmation only)

`rust_sushi`/`compiler` do **not** depend on `snapshot_gen` (verified: not in
either Cargo.toml), so the walk-engine changes cannot affect the compiler.
Best-of-3 warm `rust_sushi build` (release, isolated cache):
- **ips 0.405 s** (expected §4 ballpark 0.74 s; perf31 note has 0.385 s build)
- **mcode 0.562 s** (expected §4 ballpark 0.84 s)

Both **faster** than the §4 numbers — consistent with the merged mimalloc + CAS
materialization work from main. **No drift** (well within, in fact better than,
expected); no investigation needed. Builds exit 0 with full
`fsh-generated/resources` output (ips: 118 resources).

## Future levers (measured evidence, for the clarity/CAS pass to pick up)

1. **CAS derived-index columns (the strategic load-path fix).** Extend
   `package_acquisition`'s generated index with derived columns (`name` at
   minimum, and effectively `resourceType`) computed once at materialization,
   consumed by both `package_store` and snapshot_gen's `PackageContext`. This
   deletes per-process `probe_name` / `resourceType` scanning *and* the
   `scan_package_structure_definitions` directory-scan+sort+read entirely, and
   unifies the three parallel package-reading layers. My changes 1 & 2 are the
   interim, strictly-subsumed version of this. Measured remaining load-path cost
   after my changes (davinci-pas full batch, perf -F2500): `files.sort()`
   quicksort + `path::compare_components` ~21%, `scan_top_level_string` ~8%,
   `fs::read`/`File::open` ~7.6%, residual serde ~13% — essentially ALL of the
   ~1.4 s davinci-pas time is load/IO the CAS index removes.
2. **Walk-internal id→index maps / linear scans — REJECTED for now, no measured
   basis.** The task-brief levers (`find_end_of_element`/`get_diff_matches`
   linear scans, by-id lookups, accidental Value clones) target functions that
   register at **< 0.15% self-time each** post-optimization (`process_paths`
   0.14%, `Comparer::find` 0.14%, `finalize` 0.05%). The entire walk is ~0.6% of
   runtime. No change is justified by measurement; adding index side-maps would
   be complexity the clarity pass should reject. Revisit only if a future
   workload makes the walk (not the load) dominate.
3. **`fetch()` deep-clones the parsed base Value on every cache hit** (`fetch_rc`
   returns `Rc`, but `fetch` does `(*rc).clone()`). Measured impact tiny here
   (480 resolves/pas-batch, `Value::clone` ~2.7% and mostly emit-side), so not
   pursued — but callers that only read could take the `Rc` directly. Low-risk
   clarity cleanup, not a perf necessity.

