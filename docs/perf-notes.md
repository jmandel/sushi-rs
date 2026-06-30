# Performance notes (Phase 9 — DONE 2026-06-30)

Optimization pass complete. **Parity preserved: 665/665 byte-identical across all
4 IGs, `cargo test --workspace` green (18 suites).** Warm timings, best of 3:

| IG    | before | after | speedup |
|-------|-------:|------:|--------:|
| ips   | 14.0s  | 1.57s | 8.9x    |
| epi   |  ~1.1s | 1.11s | —       |
| mcode | 13.5s  | 2.73s | 4.9x    |
| crd   |  ~1.6s | 1.63s | —       |

IPS comfortably inside the 1.5–2.5s target (stock SUSHI ~39s).

## Original baseline / hotspot
~14s warm IPS. **~62% of CPU was `indexmap::IndexMap::get_index_of` (44%) +
SipHash (18%)**: the SD element model (`fhir_model`) stores each ElementDefinition
as a serde_json `Map` (`IndexMap` via `preserve_order`) and did string-keyed
SipHash lookups all over the hot path. The hottest were `e.id()`/`e.path()` —
called inside every linear element scan (`index_of_id`/`path_of_id`/
`find_element_by_path`/differential), profiles × rules × elements → millions.

## Changes made (each verified parity-preserving via the full 4-IG gate)

1. **Cache `id`/`path` as `String` fields on `ElementDefinition`**
   (`fhir_model/src/lib.rs`). `id()`/`path()` now return the cached field instead
   of an IndexMap+SipHash `map.get("id")`. The fields exactly mirror
   `map["id"]`/`map["path"]`; written only by `new`/`from_json`/`set_id` (the sole
   writers of those keys — verified no external `.set("id"/"path")` or `map.insert`).
   **This single change: 14s → 1.88s** (collapsed the 62% SipHash/get_index_of).

2. **Hoist `format!` out of hot filter loops** (`fhir_model` find_element_by_path
   `{fhir_path}.`/`{fhir_path}:` and the slice retain; `get_slices` prefix). These
   were rebuilding format strings per-element inside O(n) filters. → 1.77s.

3. **Lazy `FxHashMap<id,index>` side index for `index_of_id`/`path_of_id`**
   (`fhir_model`). Turns the O(n) linear scan (hence O(n²) `find_element_by_path`)
   into an O(1) cheap-hash lookup. Cache stores `(elements.len(), map)` and rebuilds
   automatically when the element count changes (covers every add/splice in the
   module); a per-lookup `elements[i].id()==id` verification self-heals position
   shifts. The one in-place id rename that doesn't change the count
   (`sd_export::reset_parent_elements`'s `set_id` loop) calls the new
   `invalidate_id_index()`. `find_element_by_path` self-time 8.3% → 2.6%. → 1.64s.

4. **Hoist two per-element `format!`s in `instance_export`** — the biggest mcode
   wins (mcode is instance-heavy: 193 instances × deep snapshots):
   - `set_implied_properties_on_instance`: `format!("{trace_path}.")` was rebuilt
     for every entry of `paths` inside `paths.iter().find()`, per BFS node
     (O(nodes×paths)). Hoisted once per node. **mcode 13.5s → 4.2s.**
   - `set_assigned_values`: `format!("{path}.")` rebuilt for every `rule_map` entry
     inside `.find()`, per rule (O(rules×rule_map)). Hoisted. **mcode 4.2s → 2.8s.**

## Remaining profile (after the pass)
- **IPS**: broadly distributed alloc churn (Vec<String>/serde_json `Value` clones in
  `find_element_by_path`'s id-matching and element clones in unfold/clone_children)
  + the FSH lexer. No single dominant symbol; further gains are diminishing.
- **mcode**: now **lexer-bound** — `fsh_lexer_parser::lex::m_ref` ~26% (out of scope
  per the task: don't touch lexer/parser behavior). Compiler/fhir_model hotspots
  reduced to single digits.

## Not pursued (diminishing returns / out of scope / risk)
- Lexer internals (m_ref) — out of scope; output is byte-exact-gated.
- COW/overlay instead of full element-array clones (lever 3 from the old plan) —
  `IndexMap clone` is only ~4–6%; high churn risk in the delicate SD/instance
  engines for modest gain. Left as future work if a bigger IG demands it.
- Parallelizing resource exports — would need care to preserve deterministic
  emission order; not needed to hit target.
- `package_store` parse cache — already done in an earlier phase.
