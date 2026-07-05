//! The `_data/*.json` site-data model builder.
//!
//! Status per file (measured vs the US Core F0 `temp/pages/_data` oracle):
//!
//! * **`artifacts.json`** — BYTE-IDENTICAL (442/442 entries). Fully derivable
//!   from the resource list: `{ "<type>-<id>.html": { "type": T, "example": true? } }`,
//!   compact JSON, keys sorted. Emitted here and byte-gated
//!   (`tests/artifacts_gate.rs`).
//! * **`structuredefinitions.json`** — field-derivable (~90%): url/name/title/
//!   kind/type/base/status/abstract/derivation/description/publisher all derive
//!   from the SD (+ IG publisher fallback). NOT yet byte-complete — see
//!   `docs/site-producer.md` §"_data gap catalog": needs (a) the publisher's
//!   JsonObject pretty-printer, (b) core-package resolution for `basename`/
//!   `basepath` when a profile's base is a core R4 type, (c) extension `contexts`,
//!   (d) the Java-`Date.toString()` timezone-formatted `date` field (run-context),
//!   (e) the special `maturities` aggregate key, (f) IG resource-order for the
//!   `index`/insertion order. Emitted best-effort behind [`structuredefinitions_json`].
//! * **`resources.json` / `pages.json` / `fhir.json` / `info.json`** — designed,
//!   with a per-field run-context gap catalog in `docs/site-producer.md`
//!   (`resources.json.source` is an absolute build path; `fhir.json` carries
//!   genDate/errorCount/tooling/counters; `info.json.copyrightyear` is the build
//!   year). Not emitted here (would not be byte-faithful without the run context).

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{json, Value};

use crate::ProducerInputs;

/// Emit the derivable `_data` files. Currently the byte-identical `artifacts.json`.
pub fn emit_data(inputs: &ProducerInputs, data: &mut BTreeMap<String, String>) -> Result<()> {
    data.insert("artifacts.json".to_string(), artifacts_json(inputs));
    Ok(())
}

/// `artifacts.json` — `{ "<type>-<id>.html": {"type": T[, "example": true]} }`,
/// compact. Key order follows the IG `definition.resource[]` processing order
/// (already applied to `inputs.resources` by `gather_inputs`). Byte-identical to
/// the publisher output for US Core.
pub fn artifacts_json(inputs: &ProducerInputs) -> String {
    let mut s = String::from("{");
    for (i, r) in inputs.resources.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        let mut o = serde_json::Map::new();
        o.insert("type".to_string(), Value::String(r.rt.clone()));
        if r.is_example {
            o.insert("example".to_string(), Value::Bool(true));
        }
        // serde_json compact serialization matches the publisher's writer:
        // {"type":"X","example":true}
        s.push_str(&serde_json::to_string(&r.base_path()).unwrap());
        s.push(':');
        s.push_str(&serde_json::to_string(&Value::Object(o)).unwrap());
    }
    s.push('}');
    s
}

/// `structuredefinitions.json` field-derivation (best-effort; not yet
/// byte-complete — see module docs). Returns the derived model as a `Value` map
/// keyed by SD id, so callers/tests can inspect field parity. `basename`/
/// `basepath` are resolved among the IG's own SDs only; a core-R4 base yields the
/// tail id + a `null` basename (a documented gap).
pub fn structuredefinitions_model(inputs: &ProducerInputs) -> Value {
    let sds: Vec<_> = inputs
        .resources
        .iter()
        .filter(|r| r.rt == "StructureDefinition")
        .collect();
    // url -> (name, id) for local base resolution
    let by_url: BTreeMap<&str, (&Option<String>, &str)> = sds
        .iter()
        .filter_map(|r| r.url.as_deref().map(|u| (u, (&r.name, r.id.as_str()))))
        .collect();

    let mut out = serde_json::Map::new();
    for (index, r) in sds.iter().enumerate() {
        let j = &r.json;
        let s = |k: &str| j.get(k).and_then(Value::as_str).map(str::to_string);
        let base = s("baseDefinition");
        let (basename, basepath) = match base.as_deref().and_then(|b| by_url.get(b)) {
            Some((n, bid)) => ((*n).clone(), Some(format!("StructureDefinition-{bid}.html"))),
            None => (None, base.as_deref().map(|b| b.rsplit('/').next().unwrap_or(b).to_string())),
        };
        let publisher = s("publisher").or_else(|| inputs.ig.publisher.clone());
        out.insert(
            r.id.clone(),
            json!({
                "index": index,
                "url": r.url,
                "name": r.name,
                "title": s("title"),
                "uml": false,
                "titlelang": {},
                "path": format!("StructureDefinition-{}.html", r.id),
                "kind": r.kind,
                "type": r.type_,
                "base": base,
                "basename": basename,
                "basepath": basepath,
                "adl": false,
                "status": s("status"),
                "abstract": r.abstract_,
                "derivation": s("derivation"),
                "publisher": publisher,
                "publisherlang": {},
                "copyright": s("copyright"),
                "copyrightlang": {},
                "description": s("description"),
                "descriptionlang": {},
                "obligations": false,
            }),
        );
    }
    Value::Object(out)
}
