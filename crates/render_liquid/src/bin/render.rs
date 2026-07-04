//! CLI driver mirroring scripts/liquid-oracle.rb's interface, so the
//! differential gate can run the Rust engine and the Ruby oracle over the same
//! (template, context, includes-dir) triple and diff byte-for-byte.
//!
//! Usage:
//!   render --template FILE --context CTX.json [--includes-dir DIR]
//!          [--data-dir DIR] [--publisher-raw-quirk]
//!
//! The context JSON is the SAME file the oracle consumes. site.data is taken
//! from `context["site"]["data"]` (or built from --data-dir with Jekyll's CSV/
//! YAML coercion, mirroring the oracle's --data-dir). Everything else in the
//! top-level context object (page, include, resource_type, ...) becomes a
//! global variable.

use render_liquid::{render_with, DataProvider, Options, Value};
use std::collections::HashMap;
use std::rc::Rc;

fn main() {
    let mut template: Option<String> = None;
    let mut context: Option<String> = None;
    let mut includes_dir: Option<String> = None;
    let mut data_dir: Option<String> = None;
    let mut raw_quirk = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--template" => template = args.next(),
            "--context" => context = args.next(),
            "--includes-dir" => includes_dir = args.next(),
            "--data-dir" => data_dir = args.next(),
            "--publisher-raw-quirk" => raw_quirk = true,
            _ => {}
        }
    }

    let src = match &template {
        Some(f) if f != "-" => std::fs::read_to_string(f).expect("read template"),
        _ => {
            use std::io::Read;
            let mut s = String::new();
            std::io::stdin().read_to_string(&mut s).unwrap();
            s
        }
    };

    let ctx: serde_json::Value = context
        .as_ref()
        .map(|f| serde_json::from_str(&std::fs::read_to_string(f).expect("read ctx")).unwrap())
        .unwrap_or(serde_json::Value::Object(Default::default()));

    // Build the provider.
    let mut prov = MapProvider::default();

    // site.data: from context or --data-dir.
    if let Some(dd) = &data_dir {
        prov.data = load_data_dir(dd);
    } else if let Some(sd) = ctx.get("site").and_then(|s| s.get("data")) {
        prov.data = json_to_value(sd);
    }
    if let Some(site) = ctx.get("site") {
        prov.site_extra = json_to_value(site);
    }

    // includes (recurse into subdirs so `whats-new/v9.md` etc. resolve, using
    // the path RELATIVE to the includes root as the include name — matching
    // Jekyll's `_includes/<relpath>` resolution).
    if let Some(dir) = &includes_dir {
        let root = std::path::Path::new(dir);
        load_includes_recursive(root, root, &mut prov.includes);
    }

    // globals = every top-level key except `site` (served by provider).
    let mut globals: Vec<(&str, Value)> = Vec::new();
    let owned: Vec<(String, Value)> = if let serde_json::Value::Object(map) = &ctx {
        map.iter()
            .filter(|(k, _)| k.as_str() != "site")
            .map(|(k, v)| (k.clone(), json_to_value(v)))
            .collect()
    } else {
        vec![]
    };
    for (k, v) in &owned {
        globals.push((k.as_str(), v.clone()));
    }

    let opts = Options {
        publisher_raw_quirk: raw_quirk,
        ..Options::default()
    };
    let out = render_with(&src, &prov, &globals, opts);
    print!("{out}");
}

#[derive(Default)]
struct MapProvider {
    data: Value,
    site_extra: Value,
    includes: HashMap<String, String>,
}

impl DataProvider for MapProvider {
    fn site_data(&self, path: &[&str]) -> Option<Value> {
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
    fn site(&self, path: &[&str]) -> Option<Value> {
        let mut cur = self.site_extra.clone();
        for seg in path {
            cur = cur.index(&Value::str(*seg));
        }
        if cur.is_nil() {
            None
        } else {
            Some(cur)
        }
    }
    fn include_source(&self, name: &str) -> Option<String> {
        self.includes.get(name).cloned()
    }
}

fn load_includes_recursive(
    root: &std::path::Path,
    dir: &std::path::Path,
    map: &mut HashMap<String, String>,
) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let path = e.path();
        // follow symlinks to files/dirs (overlay uses symlinks)
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if meta.is_dir() {
            load_includes_recursive(root, &path, map);
        } else if let Ok(body) = std::fs::read_to_string(&path) {
            if let Ok(rel) = path.strip_prefix(root) {
                map.insert(rel.to_string_lossy().replace('\\', "/"), body);
            }
        }
    }
}

fn json_to_value(j: &serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Nil,
        serde_json::Value::Bool(b) => Value::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap())
            }
        }
        serde_json::Value::String(s) => Value::str(s.clone()),
        serde_json::Value::Array(a) => Value::array(a.iter().map(json_to_value).collect()),
        serde_json::Value::Object(o) => {
            let mut m = render_liquid::OrderedMap::new();
            for (k, v) in o {
                m.insert(k.clone(), json_to_value(v));
            }
            Value::Hash(Rc::new(m))
        }
    }
}

/// Mirror Jekyll's data_reader for --data-dir: CSV -> array of row-hashes
/// keyed by header; YAML/JSON as-is (JSON only here to avoid a YAML dep — the
/// gate's --data-dir fixtures use CSV/JSON; YAML data uses --context instead).
fn load_data_dir(dir: &str) -> Value {
    let mut m = render_liquid::OrderedMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Value::Nil;
    };
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let key = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let key = sanitize(&key);
        match ext {
            "csv" => {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    m.insert(key, csv_to_value(&text));
                }
            }
            "json" => {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    if let Ok(j) = serde_json::from_str::<serde_json::Value>(&text) {
                        m.insert(key, json_to_value(&j));
                    }
                }
            }
            _ => {}
        }
    }
    Value::Hash(Rc::new(m))
}

fn sanitize(name: &str) -> String {
    // Jekyll sanitize_filename: strip non-word chars, spaces->_.
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-' || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_")
}

/// Minimal RFC-4180 CSV parser matching Ruby CSV.read(headers:true).to_hash:
/// array of hashes, each keyed by the header row. Handles quoted fields with
/// embedded commas/quotes/newlines.
fn csv_to_value(text: &str) -> Value {
    let rows = parse_csv(text);
    if rows.is_empty() {
        return Value::array(vec![]);
    }
    let headers = &rows[0];
    let mut out = Vec::new();
    for row in &rows[1..] {
        let mut m = render_liquid::OrderedMap::new();
        for (i, h) in headers.iter().enumerate() {
            let v = row.get(i).cloned().unwrap_or_default();
            // Ruby CSV yields nil for missing/empty-unquoted trailing cells;
            // to_hash keeps empty string for present-but-empty cells. We store
            // "" (empty string) which the `where`/`if` semantics treat as
            // present-but-empty — matching observed behavior.
            m.insert(h.clone(), Value::str(v));
        }
        out.push(Value::Hash(Rc::new(m)));
    }
    Value::array(out)
}

fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut field = String::new();
    let mut record: Vec<String> = Vec::new();
    let mut in_quotes = false;
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_quotes {
            if c == '"' {
                if i + 1 < bytes.len() && bytes[i + 1] == '"' {
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
        } else {
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
                    i += 1;
                }
                '\n' => {
                    record.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut record));
                    i += 1;
                }
                _ => {
                    field.push(c);
                    i += 1;
                }
            }
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        rows.push(record);
    }
    rows
}
