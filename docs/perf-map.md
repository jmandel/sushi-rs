# Perf opportunity map (by area) — for distinct-lane agent assignment

Profiled self-time by subsystem (perf -F2500), post-m_ref, HEAD 8b7fb77:

| Area | ips | mcode | crd | Notes |
|---|--:|--:|--:|---|
| serde_json PARSE (skip_to_escape/ignore_str) | ~6% | ~6% | **37%** | crd: `ignore_str` = package_store eager name-probe deserializing every SD/VS/CS while skipping other fields |
| allocator (malloc/realloc/free/RawVec) | 15% | 12% | 8% | transient String/Vec/clone churn |
| hashing (SipHash) + indexmap ops | ~16% | ~16% | ~8% | serde_json element-map get/insert/clone with default SipHash |
| fmt/format! | 5.5% | 4.5% | 1% | string building in hot loops |
| instance_export | 1.7% | 7.6% | 0.5% | mcode instance-heavy assignment engine |
| fhir_model | 5.3% | 4% | 1% | find_element_by_path/unfold/diff |

## Distinct lanes (no file overlap)
- **A = parse/IO**: `package_store` eager name-probe → targeted byte-scan for top-level `"name"` (or lazy name index) to kill `ignore_str`/`skip_to_escape`. crd's 37%. Also check for any redundant base-SD re-parses. Files: package_store.
- **B = structural element clones**: Rc/COW for read-shared element maps → cut `IndexMap::clone` (~8%) + serde_json Value clone/drop. Files: fhir_model clone/unfold/diff. Do NOT replace serde_json::Value wholesale (unmergeable).
- **C = transient allocations + fmt**: String/Vec/format! churn in hot loops (allocator ~15% + fmt ~5%). Files: hot loops in fhir_model/sd_export/instance_export/paths. Avoid B's clone structure + A's parsing.

SipHash on serde maps (~16%) is the deepest lever but needs a custom value type / hasher (mostly unmergeable). Partly addressed by B (fewer clones → fewer maps hashed). Revisit as a stretch later.
