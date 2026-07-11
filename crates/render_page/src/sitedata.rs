//! `SiteData` — the `site.data.*` surface, loaded from the build's `_data/*.json`.
//!
//! In the publisher's Jekyll tree, `temp/pages/_data/<name>.json` is exposed as
//! `site.data.<name>`. For the F5 gate we read those files DIRECTLY (the same
//! faithful-oracle-input pattern the fragment gates use for `output/` SDs).
//!
//! Each `_data/<name>.json` becomes one key under `site.data`. The values are
//! converted from serde_json into render_liquid `Value`s (insertion-ordered
//! Hashes — `serde_json` is built with `preserve_order`, so key order is the
//! file's order, matching Ruby's `site.data` load).

use std::collections::BTreeMap;
use std::path::Path;

use render_liquid::{OrderedMap, Value};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SiteDataLoadError {
    #[error("read site data directory {path}: {source}")]
    ReadDirectory {
        path: String,
        source: std::io::Error,
    },
    #[error("inspect site data entry {path}: {source}")]
    Inspect {
        path: String,
        source: std::io::Error,
    },
    #[error("site data entry is not a regular file: {0}")]
    NotRegular(String),
    #[error("site data filename is not UTF-8: {0}")]
    NonUtf8Name(String),
    #[error("read site data file {path}: {source}")]
    ReadFile {
        path: String,
        source: std::io::Error,
    },
    #[error("site data file {path} is not UTF-8: {source}")]
    NonUtf8Body {
        path: String,
        source: std::string::FromUtf8Error,
    },
    #[error("parse site data file {path}: {message}")]
    Parse { path: String, message: String },
    #[error("site data key {key} is declared by both {first} and {second}")]
    DuplicateKey {
        key: String,
        first: String,
        second: String,
    },
}

/// The whole `site.data` namespace as one render_liquid Hash.
pub struct SiteData {
    data: Value,
    /// Liquid top-level key -> exact `_data` filename. This lets the page
    /// collector retain the file-level SiteBuild dependency despite values
    /// being parsed into one in-memory object.
    sources: BTreeMap<String, (String, Vec<u8>)>,
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
        Self::load_with_tree(&render_sd::tree::FsTree, data_dir)
    }

    /// Fail-closed native loader used by revision-producing hosts. Unlike the
    /// compatibility loader, it propagates directory/read/parse errors, rejects
    /// symlinks/nested entries the current flat model cannot represent, and
    /// rejects two extensions that collapse to the same Liquid key.
    pub fn load_strict(data_dir: &Path) -> Result<SiteData, SiteDataLoadError> {
        let directory =
            std::fs::read_dir(data_dir).map_err(|source| SiteDataLoadError::ReadDirectory {
                path: data_dir.display().to_string(),
                source,
            })?;
        let mut entries = directory
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|source| SiteDataLoadError::ReadDirectory {
                path: data_dir.display().to_string(),
                source,
            })?;
        entries.sort_by_key(|entry| entry.file_name());

        let mut root = OrderedMap::new();
        let mut sources = BTreeMap::new();
        let mut key_sources = BTreeMap::<String, String>::new();
        for entry in entries {
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|source| SiteDataLoadError::Inspect {
                    path: path.display().to_string(),
                    source,
                })?;
            if file_type.is_symlink() || file_type.is_dir() || !file_type.is_file() {
                return Err(SiteDataLoadError::NotRegular(path.display().to_string()));
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|name| SiteDataLoadError::NonUtf8Name(name.to_string_lossy().into()))?;
            let extension = Path::new(&name)
                .extension()
                .and_then(|value| value.to_str());
            if !matches!(extension, Some("json" | "yml" | "yaml" | "csv")) {
                continue;
            }
            let key = Path::new(&name)
                .file_stem()
                .and_then(|value| value.to_str())
                .expect("UTF-8 filename has UTF-8 stem")
                .to_string();
            if let Some(first) = key_sources.insert(key.clone(), name.clone()) {
                return Err(SiteDataLoadError::DuplicateKey {
                    key,
                    first,
                    second: name,
                });
            }
            let bytes = std::fs::read(&path).map_err(|source| SiteDataLoadError::ReadFile {
                path: path.display().to_string(),
                source,
            })?;
            let text =
                String::from_utf8(bytes).map_err(|source| SiteDataLoadError::NonUtf8Body {
                    path: path.display().to_string(),
                    source,
                })?;
            let parsed = match extension {
                Some("json") => {
                    serde_json::from_str::<serde_json::Value>(&text).map_err(|error| {
                        SiteDataLoadError::Parse {
                            path: path.display().to_string(),
                            message: error.to_string(),
                        }
                    })?
                }
                Some("csv") => csv_to_json(&text).map_err(|message| SiteDataLoadError::Parse {
                    path: path.display().to_string(),
                    message,
                })?,
                Some("yml" | "yaml") => {
                    serde_yaml::from_str::<serde_json::Value>(&text).map_err(|error| {
                        SiteDataLoadError::Parse {
                            path: path.display().to_string(),
                            message: error.to_string(),
                        }
                    })?
                }
                _ => unreachable!(),
            };
            sources.insert(key.clone(), (name, text.into_bytes()));
            root.insert(key, json_to_value(&parsed));
        }
        Ok(SiteData {
            data: Value::Hash(std::rc::Rc::new(root)),
            sources,
        })
    }

    /// Tree-parameterized load (MemTree in the wasm session).
    pub fn load_with_tree(tree: &dyn render_sd::tree::TreeSource, data_dir: &Path) -> SiteData {
        let mut root = OrderedMap::new();
        let mut sources = BTreeMap::new();
        let mut names: Vec<String> = tree
            .read_dir(data_dir)
            .into_iter()
            .flatten()
            .filter(|(_, is_file)| *is_file)
            .map(|(n, _)| n)
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
            let Some(text) = tree.read(&path) else {
                continue;
            };
            let (key, parsed) = if n.ends_with(".json") {
                (
                    n.trim_end_matches(".json").to_string(),
                    serde_json::from_str::<serde_json::Value>(&text).ok(),
                )
            } else if n.ends_with(".csv") {
                (
                    n.trim_end_matches(".csv").to_string(),
                    csv_to_json(&text).ok(),
                )
            } else {
                let key = n
                    .trim_end_matches(".yaml")
                    .trim_end_matches(".yml")
                    .to_string();
                (key, serde_yaml::from_str::<serde_json::Value>(&text).ok())
            };
            if let Some(v) = parsed {
                sources.insert(key.clone(), (n, text.into_bytes()));
                root.insert(key, json_to_value(&v));
            }
        }
        SiteData {
            data: Value::Hash(std::rc::Rc::new(root)),
            sources,
        }
    }

    /// From an already-built serde_json object mapping data-key -> json (for the
    /// editor path / tests where _data files aren't on disk).
    pub fn from_map(map: &serde_json::Value) -> SiteData {
        let sources = map
            .as_object()
            .into_iter()
            .flat_map(|object| object.keys())
            .map(|key| {
                let bytes = map
                    .get(key)
                    .and_then(|value| serde_json::to_vec(value).ok())
                    .unwrap_or_default();
                (key.clone(), (key.clone(), bytes))
            })
            .collect();
        SiteData {
            data: json_to_value(map),
            sources,
        }
    }

    pub fn source_name(&self, data_key: &str) -> Option<&str> {
        self.sources.get(data_key).map(|(name, _)| name.as_str())
    }

    pub fn source_bytes(&self, data_key: &str) -> Option<&[u8]> {
        self.sources
            .get(data_key)
            .map(|(_, bytes)| bytes.as_slice())
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
fn csv_to_json(text: &str) -> Result<serde_json::Value, String> {
    let records = parse_csv(text)?;
    let mut rows = Vec::new();
    let Some(header) = records.first() else {
        return Ok(serde_json::Value::Array(rows));
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
                // Ruby `CSV.read(headers: true)` yields nil (NOT "") for an empty
                // cell. This is load-bearing: in Liquid `nil` is falsy but `""`
                // is TRUTHY, so `{% if row.col %}` skips empty cells only if they
                // are nil. (US-Core's search-requirement handler relies on this —
                // empty multipleAnd_conf/comparator cells must suppress their
                // `- Including …` bullets, which also keeps the list loose.)
                let v = if cell.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(cell.clone())
                };
                obj.insert(key.clone(), v);
            }
        }
        rows.push(serde_json::Value::Object(obj));
    }
    Ok(serde_json::Value::Array(rows))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CsvState {
    Start,
    Unquoted,
    Quoted,
    AfterQuote,
}

/// RFC-4180 tokenizer: returns a Vec of records, each a Vec of field strings.
fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut records = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut state = CsvState::Start;
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match state {
            CsvState::Quoted => {
                if c == '"' {
                    if chars.get(i + 1) == Some(&'"') {
                        field.push('"');
                        i += 2;
                    } else {
                        state = CsvState::AfterQuote;
                        i += 1;
                    }
                } else {
                    field.push(c);
                    i += 1;
                }
            }
            CsvState::AfterQuote => match c {
                ',' => {
                    record.push(std::mem::take(&mut field));
                    state = CsvState::Start;
                    i += 1;
                }
                '\r' | '\n' => finish_csv_record(
                    &chars,
                    &mut i,
                    &mut state,
                    &mut field,
                    &mut record,
                    &mut records,
                ),
                _ => {
                    return Err(format!(
                        "illegal character after closing quote at character {}",
                        i + 1
                    ));
                }
            },
            CsvState::Start => match c {
                '"' => {
                    state = CsvState::Quoted;
                    i += 1;
                }
                ',' => {
                    record.push(String::new());
                    i += 1;
                }
                '\r' | '\n' => finish_csv_record(
                    &chars,
                    &mut i,
                    &mut state,
                    &mut field,
                    &mut record,
                    &mut records,
                ),
                _ => {
                    field.push(c);
                    state = CsvState::Unquoted;
                    i += 1;
                }
            },
            CsvState::Unquoted => match c {
                '"' => {
                    return Err(format!(
                        "illegal quote in unquoted field at character {}",
                        i + 1
                    ));
                }
                ',' => {
                    record.push(std::mem::take(&mut field));
                    state = CsvState::Start;
                    i += 1;
                }
                '\r' | '\n' => finish_csv_record(
                    &chars,
                    &mut i,
                    &mut state,
                    &mut field,
                    &mut record,
                    &mut records,
                ),
                _ => {
                    field.push(c);
                    i += 1;
                }
            },
        }
    }
    if state == CsvState::Quoted {
        return Err("unclosed quoted field at end of file".into());
    }
    // Flush a final field/record if the file did not end with a newline.
    if state != CsvState::Start || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    Ok(records)
}

fn finish_csv_record(
    chars: &[char],
    index: &mut usize,
    state: &mut CsvState,
    field: &mut String,
    record: &mut Vec<String>,
    records: &mut Vec<Vec<String>>,
) {
    record.push(std::mem::take(field));
    records.push(std::mem::take(record));
    if chars[*index] == '\r' && chars.get(*index + 1) == Some(&'\n') {
        *index += 2;
    } else {
        *index += 1;
    }
    *state = CsvState::Start;
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
        let v = csv_to_json(text).unwrap();
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
        // empty-string keys; a data row can be shorter than the header list. An
        // empty cell becomes JSON null (Ruby CSV nil), NOT "".
        let text = "a,b,\r\n1,,\r\n";
        let v = csv_to_json(text).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["a"], serde_json::json!("1"));
        assert_eq!(arr[0]["b"], serde_json::Value::Null); // empty cell -> nil
        assert_eq!(arr[0][""], serde_json::Value::Null); // padding column -> nil
    }

    #[test]
    fn strict_csv_rejects_unclosed_and_illegal_quotes() {
        for (name, body, expected) in [
            (
                "unclosed.csv",
                "key,value\na,\"open",
                "unclosed quoted field",
            ),
            ("illegal.csv", "key,value\na,b\"ad", "illegal quote"),
            (
                "after.csv",
                "key,value\na,\"closed\"suffix",
                "after closing quote",
            ),
        ] {
            let temp = tempfile::tempdir().unwrap();
            std::fs::write(temp.path().join(name), body).unwrap();
            let error = SiteData::load_strict(temp.path())
                .err()
                .expect("malformed CSV must fail")
                .to_string();
            assert!(error.contains(expected), "{error}");
        }
    }
}
