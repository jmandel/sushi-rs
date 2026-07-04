//! `SiteData` — the `site.data.*` surface, loaded from the build's `_data/*.json`.
//!
//! In the publisher's Jekyll tree, `temp/pages/_data/<name>.json` is exposed as
//! `site.data.<name>`. For the F5 gate we read those files DIRECTLY (the same
//! faithful-oracle-input pattern the fragment gates use for `output/` SDs); the
//! editor's site_db → _data mapping is documented separately.
//!
//! Each `_data/<name>.json` becomes one key under `site.data`. The values are
//! converted from serde_json into render_liquid `Value`s (insertion-ordered
//! Hashes — `serde_json` is built with `preserve_order`, so key order is the
//! file's order, matching Ruby's `site.data` load).

use std::path::Path;

use render_liquid::{OrderedMap, Value};

/// The whole `site.data` namespace as one render_liquid Hash.
pub struct SiteData {
    data: Value,
}

impl SiteData {
    /// Load every data file in `data_dir` as `site.data.<name>`. Mirrors
    /// Jekyll's `_data` loading: `.json` and `.yml`/`.yaml` files each become
    /// `site.data.<basename>` (the extension stripped). Keys are inserted in
    /// filename-sorted order (Jekyll loads _data alphabetically). CSV files are
    /// not loaded here — no page/include template in the corpus reads a
    /// CSV-backed `site.data.*` key (measured); a loud gap would surface one.
    pub fn load(data_dir: &Path) -> SiteData {
        let mut root = OrderedMap::new();
        let mut names: Vec<String> = std::fs::read_dir(data_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(".json") || n.ends_with(".yml") || n.ends_with(".yaml"))
            .collect();
        names.sort();
        for n in names {
            let path = data_dir.join(&n);
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let (key, parsed) = if n.ends_with(".json") {
                (
                    n.trim_end_matches(".json").to_string(),
                    serde_json::from_str::<serde_json::Value>(&text).ok(),
                )
            } else {
                let key = n.trim_end_matches(".yaml").trim_end_matches(".yml").to_string();
                (key, serde_yaml::from_str::<serde_json::Value>(&text).ok())
            };
            if let Some(v) = parsed {
                root.insert(key, json_to_value(&v));
            }
        }
        SiteData { data: Value::Hash(std::rc::Rc::new(root)) }
    }

    /// From an already-built serde_json object mapping data-key -> json (for the
    /// editor path / tests where _data files aren't on disk).
    pub fn from_map(map: &serde_json::Value) -> SiteData {
        SiteData { data: json_to_value(map) }
    }

    pub fn site_data(&self, path: &[&str]) -> Option<Value> {
        let mut cur = self.data.clone();
        for seg in path {
            cur = cur.index(&Value::str(*seg));
        }
        if cur.is_nil() {
            None
        } else {
            Some(cur)
        }
    }

    /// `site.<key>` beyond `site.data` (title/pages/…). The FHIR template's page
    /// inputs read only `site.data.*`; other `site.*` keys are nil here.
    pub fn site(&self, _path: &[&str]) -> Option<Value> {
        None
    }
}

/// serde_json -> render_liquid Value. Integers stay Int; non-integer numbers
/// Float; objects become insertion-ordered Hashes (preserve_order feature).
pub fn json_to_value(v: &serde_json::Value) -> Value {
    match v {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else if let Some(f) = n.as_f64() {
                Value::Float(f)
            } else {
                Value::Nil
            }
        }
        serde_json::Value::String(s) => Value::str(s.as_str()),
        serde_json::Value::Array(a) => Value::array(a.iter().map(json_to_value).collect()),
        serde_json::Value::Object(o) => {
            let mut m = OrderedMap::new();
            for (k, val) in o {
                m.insert(k.clone(), json_to_value(val));
            }
            Value::Hash(std::rc::Rc::new(m))
        }
    }
}
