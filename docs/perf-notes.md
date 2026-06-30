# Performance notes (Phase 9 backlog)

Optimization is deferred until parity is complete (per the plan). Findings so far,
measured on a warm IPS build (`rust_sushi build temp/ips-ig`):

## Baseline
- ~14s warm (partial pipeline). `package_store` construction = 0.11s (NOT the cost).
- Stock SUSHI ~39s; our target 1.5–2.5s.

## Profiling (perf, IPS build)
**~62% of CPU is `indexmap::IndexMap::get_index_of` (44%) + SipHash hashing (18%).**
Other notable: `fhir_model::differential_elements` 2.3%, `path_of_id` 2.0%.

Root cause: the SD element model (`fhir_model`) stores each ElementDefinition as a
serde_json `Map` (= `IndexMap` via `preserve_order`) and does **string-keyed
lookups with the default SipHash** all over the hot path (path_of_id,
findElementByPath, differential, calculateDiff). Snapshots have hundreds of
elements; profiles × rules × elements → millions of hashed string lookups.

## Phase 9 levers (in priority order)
1. **Faster hashing / fewer lookups in `fhir_model`**: index elements by id in a
   side `FxHashMap` (cheap hash), avoid re-`get_index_of` on every field access,
   cache `path_of_id`. Biggest single win (kills the 62%).
2. **`package_store` parse cache** — DONE (memoize file read+parse; opens
   10276→7825). Marginal on warm IPS (parsing wasn't the bottleneck) but matters
   for cold cache / bigger IGs.
3. Avoid `cloneDeep`-style full clones of element arrays where COW/overlay works.
4. Parallelize independent resource exports after deterministic parity (plan §Parallelism).

Re-profile after #1; the IndexMap/SipHash cost should collapse.
