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
    /// Jekyll's `_data` loading: `.json`, `.yml`/`.yaml` AND `.csv` files each
    /// become `site.data.<basename>` (the extension stripped). Keys are inserted
    /// in filename-sorted order (Jekyll loads _data alphabetically).
    ///
    /// CSV: Jekyll reads a `_data/*.csv` via Ruby `CSV.read(path, headers: true)`
    /// and exposes it as an Array of Hashes (header row → keys; each data row →
    /// `{header => cell}`). The US-Core template drives its CONF link-reference
    /// definitions (`requirements-link-list.md`) and its requirements/uscdi/
    /// vsacname tables entirely from these CSVs, so loading them here closes the
    /// bulk of the us-core page residuals (they are the ONLY `site.data` source
    /// for those Liquid loops — no publisher-side injection is involved).
    pub fn load(data_dir: &Path) -> SiteData {
        let mut root = OrderedMap::new();
        let mut names: Vec<String> = std::fs::read_dir(data_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| {
                n.ends_with(".json")
                    || n.ends_with(".yml")
                    || n.ends_with(".yaml")
                    || n.ends_with(".csv")
            })
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
            } else if n.ends_with(".csv") {
                (n.trim_end_matches(".csv").to_string(), Some(csv_to_json(&text)))
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

/// Parse a CSV document the way Jekyll's `CSV.read(path, headers: true)` exposes
/// it: an Array of row Hashes. The first record is the header row; each later
/// record becomes an object mapping `header[i] => cell[i]`. Cells are kept as
/// strings (Ruby CSV yields strings; Liquid's `where_exp`/`.size` operate on
/// them as such). A row with fewer cells than headers leaves the missing keys
/// absent (Ruby CSV → nil / Liquid nil, matching `item.key.size > 0` filtering);
/// extra cells beyond the header count are dropped (Ruby maps them under a nil
/// key, which no template reads). Blank trailing header names (e.g.
/// `uscdi-table.csv`'s padding columns) become empty-string keys, harmlessly.
///
/// RFC-4180 quoting: a field wrapped in `"…"` may contain commas, CRLF and
/// doubled `""` (→ a literal `"`). Record separators are CRLF or LF. A trailing
/// newline does NOT produce an empty final record.
fn csv_to_json(text: &str) -> serde_json::Value {
    let records = parse_csv(text);
    let mut rows = Vec::new();
    let Some(header) = records.first() else {
        return serde_json::Value::Array(rows);
    };
    for rec in records.iter().skip(1) {
        // Skip a wholly-empty trailing record (Ruby CSV skips a bare final CRLF,
        // but a line of only commas is a real record; guard just the empty vec).
        if rec.len() == 1 && rec[0].is_empty() {
            continue;
        }
        let mut obj = serde_json::Map::new();
        for (i, key) in header.iter().enumerate() {
            if let Some(cell) = rec.get(i) {
                obj.insert(key.clone(), serde_json::Value::String(cell.clone()));
            }
        }
        rows.push(serde_json::Value::Object(obj));
    }
    serde_json::Value::Array(rows)
}

/// RFC-4180 tokenizer: returns a Vec of records, each a Vec of field strings.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_quotes {
            if c == '"' {
                if chars.get(i + 1) == Some(&'"') {
                    field.push('"');
                    i += 2;
                    continue;
                }
                in_quotes = false;
                i += 1;
                continue;
            }
            field.push(c);
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_quotes = true;
                i += 1;
            }
            ',' => {
                record.push(std::mem::take(&mut field));
                i += 1;
            }
            '\r' => {
                // CRLF or lone CR → record boundary.
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                if chars.get(i + 1) == Some(&'\n') {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                i += 1;
            }
            _ => {
                field.push(c);
                i += 1;
            }
        }
    }
    // Flush a final field/record if the file did not end with a newline.
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_headers_and_quoting() {
        // Header row → keys; quoted fields carry commas + doubled-quote escapes;
        // CRLF and LF both terminate records; a short row omits trailing keys.
        let text = "key,requirement,conformance\r\n\
            CONF-0001,\"To support a Profile, a Server:\",SHALL\r\n\
            CONF-0002,\"He said \"\"hi\"\"\",MAY\n\
            CONF-0003,short\n";
        let v = csv_to_json(text);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["key"], serde_json::json!("CONF-0001"));
        assert_eq!(
            arr[0]["requirement"],
            serde_json::json!("To support a Profile, a Server:")
        );
        assert_eq!(arr[0]["conformance"], serde_json::json!("SHALL"));
        assert_eq!(arr[1]["requirement"], serde_json::json!("He said \"hi\""));
        // Short row: the missing "conformance" key is absent (Liquid nil).
        assert_eq!(arr[2]["requirement"], serde_json::json!("short"));
        assert!(arr[2].get("conformance").is_none());
    }

    #[test]
    fn csv_empty_and_padding_headers() {
        // uscdi-table.csv style: trailing empty-name header columns are kept as
        // empty-string keys; a data row can be shorter than the header list.
        let text = "a,b,\r\n1,2,\r\n";
        let v = csv_to_json(text);
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["a"], serde_json::json!("1"));
        assert_eq!(arr[0]["b"], serde_json::json!("2"));
        assert_eq!(arr[0][""], serde_json::json!(""));
    }
}
