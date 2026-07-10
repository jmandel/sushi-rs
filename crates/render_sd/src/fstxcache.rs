//! `render_sd::fstxcache` — the filesystem [`TxCacheSource`] implementor, backed
//! by a build's `input-cache/txcache` directory. THIS is the only place that
//! touches `std::fs`; the renderer talks to the [`TxCacheSource`] trait so the
//! editor's OPFS cache can implement the same seam.
//!
//! Cache formats read here (see `txcache.rs` module docs):
//!   - `*.cache`: `-`-delimited request/response blocks; `<req-json>####<tag>: <resp>`.
//!     An **expand** request (`e:`) carries `valueSet.compose`; response has
//!     `valueSet.expansion`. A **validate-code** request (`v:`) carries a `code`
//!     coding; response has a `display`.
//!   - `cs-externals.json` / `vs-externals.json`: canonical → { server, filename }.
//!
//! For **internal** expansions (the golden's "Expansion performed internally
//! based on ..."), no cache entry is needed: we enumerate the IG-owned
//! CodeSystem the include references and synthesise the expansion + the
//! `used-codesystem` version parameter, exactly as the publisher's local
//! expander would.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::context::IgContext;
use crate::txcache::{ExpandedValueSet, TxCacheSource};

pub struct FsTxCache<'a> {
    dir: Option<PathBuf>,
    ctx: &'a IgContext,
    /// Parsed expand blocks: keyed loosely (we scan on demand).
    cache_files: Vec<PathBuf>,
}

impl<'a> FsTxCache<'a> {
    pub fn new(dir: Option<&Path>, ctx: &'a IgContext) -> FsTxCache<'a> {
        let mut cache_files = Vec::new();
        if let Some(d) = dir {
            if let Some(rd) = ctx.tree().read_dir(d) {
                for (name, is_file) in rd {
                    if !is_file {
                        continue;
                    }
                    let p = d.join(&name);
                    if p.extension().and_then(|x| x.to_str()) == Some("cache") {
                        cache_files.push(p);
                    }
                }
            }
            cache_files.sort();
        }
        FsTxCache {
            dir: dir.map(|d| d.to_path_buf()),
            ctx,
            cache_files,
        }
    }

    /// Enumerate an IG-owned (or loaded) CodeSystem into an internal expansion:
    /// each concept → a contains entry (system/code/display, +definition via the
    /// definition column path), plus a `used-codesystem` param carrying the CS
    /// version. Returns None if the CS is not a local complete CodeSystem.
    fn internal_expand(&self, vs_json: &Value) -> Option<ExpandedValueSet> {
        let compose = vs_json.get("compose")?;
        let includes = compose.get("include")?.as_array()?;
        // Internal only when every include is a bare (concept-list or all-codes)
        // reference to a local complete CodeSystem, no filters, no valueSet.
        let mut contains: Vec<Value> = Vec::new();
        // The publisher expands `withLanguage("en")` → a displayLanguage param
        // (drives the "Display (en)" column header).
        let mut params: Vec<Value> = vec![serde_json::json!({
            "name": "displayLanguage",
            "valueCode": "en"
        })];
        let mut seen_params: Vec<String> = Vec::new();
        // property definitions collected across includes (code -> uri).
        let mut prop_defs: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for inc in includes {
            let system = inc.get("system").and_then(|x| x.as_str())?;
            if inc
                .get("filter")
                .and_then(|f| f.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false)
            {
                return None;
            }
            if inc.get("valueSet").is_some() {
                return None;
            }
            let ver = inc.get("version").and_then(|x| x.as_str());
            let canonical = match ver {
                Some(v) if !v.is_empty() => format!("{}|{}", system, v),
                _ => system.to_string(),
            };
            // A tx-fetched external CS (cs-externals.json) is the publisher's
            // findTxResource copy and wins over any package placeholder (which is
            // typically content=not-present and would abort the enumeration).
            let cs_json = self
                .ctx
                .resolve_cs_external(system)
                .map(|(_, j)| j)
                .or_else(|| self.ctx.load_resource(&canonical))
                .or_else(|| self.ctx.load_resource(system))?;
            let content = cs_json.get("content").and_then(|x| x.as_str());
            if !matches!(content, Some("complete") | Some("fragment")) {
                return None;
            }
            let cs_version = cs_json
                .get("version")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            // Collect the CS property definitions (code -> uri) for the expansion
            // property columns (only `status` and other displayable props matter).
            collect_cs_property_defs(&cs_json, &mut prop_defs);
            // used-codesystem param: "{system}|{version}".
            let param_val = format!("{}|{}", system, cs_version);
            if !seen_params.contains(&param_val) {
                seen_params.push(param_val.clone());
                params.push(serde_json::json!({
                    "name": "used-codesystem",
                    "valueUri": param_val
                }));
            }

            // Which concepts: explicit concept list, else all concepts (recursively).
            if let Some(concepts) = inc.get("concept").and_then(|x| x.as_array()) {
                for c in concepts {
                    let code = c.get("code").and_then(|x| x.as_str()).unwrap_or("");
                    // display: the include concept's display, else the CS concept's.
                    let display = c
                        .get("display")
                        .and_then(|x| x.as_str())
                        .map(String::from)
                        .or_else(|| cs_lookup_display(&cs_json, code));
                    let cprops = cs_concept_props(&cs_json, code);
                    contains.push(mk_contains(
                        system,
                        cs_version,
                        code,
                        display.as_deref(),
                        &cprops,
                    ));
                }
            } else {
                // all codes: enumerate the CS concepts in order.
                let hier = hierarchy_prop_codes(&cs_json);
                if let Some(cs_concepts) = cs_json.get("concept").and_then(|x| x.as_array()) {
                    enumerate_cs(cs_concepts, system, cs_version, &hier, &mut contains);
                }
            }
        }
        // Apply excludes (concept lists only; a filtered/valueSet exclude falls
        // back to the cache path). Remove excluded (system, code) pairs.
        if let Some(excludes) = compose.get("exclude").and_then(|x| x.as_array()) {
            for exc in excludes {
                let esys = exc.get("system").and_then(|x| x.as_str());
                if exc.get("filter").is_some() || exc.get("valueSet").is_some() {
                    return None; // not locally enumerable
                }
                if let Some(ecodes) = exc.get("concept").and_then(|x| x.as_array()) {
                    let codes: Vec<&str> = ecodes
                        .iter()
                        .filter_map(|c| c.get("code").and_then(|x| x.as_str()))
                        .collect();
                    contains.retain(|c| {
                        let csys = c.get("system").and_then(|x| x.as_str());
                        let ccode = c.get("code").and_then(|x| x.as_str()).unwrap_or("");
                        !(csys == esys && codes.contains(&ccode))
                    });
                } else {
                    // exclude all codes of a system → drop them.
                    contains.retain(|c| c.get("system").and_then(|x| x.as_str()) != esys);
                }
            }
        }
        // The publisher's local expander reports a total (== the flat count).
        let total = count_flat(&contains) as i64;
        // expansion.property[] = the collected {code, uri} defs (status etc.),
        // but only those that some contains entry actually carries a value for.
        let properties: Vec<Value> = prop_defs
            .into_iter()
            .filter(|(code, _)| contains_any_prop(&contains, code))
            .map(|(code, uri)| serde_json::json!({"code": code, "uri": uri}))
            .collect();
        Some(ExpandedValueSet {
            contains,
            parameters: params,
            total: Some(total),
            source: Some("internal".to_string()),
            properties,
        })
    }

    /// Read a cached `$expand` response for this VS from the `.cache` files.
    /// Matches the request block whose `valueSet.compose.include` equals this
    /// VS's compose includes (system + concept codes). Returns the response's
    /// `expansion` (contains + parameters), source = "tx.fhir.org"-style server
    /// name derived from the response `server`/`source` field.
    fn cache_expand(&self, vs_json: &Value) -> Option<ExpandedValueSet> {
        let compose = vs_json.get("compose")?;
        let want_includes = compose.get("include")?.as_array()?;
        // Fingerprint: sorted (system, sorted codes) for each include.
        let want_fp = compose_fingerprint(want_includes);

        for cf in &self.cache_files {
            let Some(text) = self.ctx.tree().read(cf) else {
                continue;
            };
            for (req, tag, resp) in parse_cache_blocks(&text) {
                if tag != "e" {
                    continue;
                }
                // req has {"valueSet": {compose...}} (+ maybe "hierarchical").
                let Some(req_v) = serde_json::from_str::<Value>(&req).ok() else {
                    continue;
                };
                let Some(req_vs) = req_v.get("valueSet") else {
                    continue;
                };
                let Some(req_inc) = req_vs
                    .get("compose")
                    .and_then(|c| c.get("include"))
                    .and_then(|x| x.as_array())
                else {
                    continue;
                };
                if compose_fingerprint(req_inc) != want_fp {
                    continue;
                }
                // The cached `e:` response envelope is NOT strict JSON — it has a
                // bare `"source" : tx.fhir.org` (unquoted). Extract just the inner
                // `"valueSet": { ... }` object (valid JSON) by brace-matching.
                let Some(vsv) = extract_json_object(&resp, "\"valueSet\"") else {
                    continue;
                };
                let Some(expansion) = vsv.get("expansion") else {
                    continue;
                };
                let contains = expansion
                    .get("contains")
                    .and_then(|x| x.as_array())
                    .cloned()
                    .unwrap_or_default();
                let mut parameters = expansion
                    .get("parameter")
                    .and_then(|x| x.as_array())
                    .cloned()
                    .unwrap_or_default();
                // The publisher expands `withLanguage("en")`; expandVS stamps a
                // `displayLanguage` param on the returned expansion (drives the
                // "Display (en)" column). The cached server response may omit it,
                // so inject it if absent (matches the publisher's post-expand VS).
                if !parameters
                    .iter()
                    .any(|p| p.get("name").and_then(|x| x.as_str()) == Some("displayLanguage"))
                {
                    parameters
                        .push(serde_json::json!({"name": "displayLanguage", "valueCode": "en"}));
                }
                let total = expansion.get("total").and_then(|x| x.as_i64());
                // server source: the bare `"source" : <host>` line in the envelope.
                let server = extract_bare_source(&resp)
                    .map(|s| strip_scheme(&s))
                    .unwrap_or_else(|| "tx.fhir.org".to_string());
                let properties = expansion
                    .get("property")
                    .and_then(|x| x.as_array())
                    .cloned()
                    .unwrap_or_default();
                return Some(ExpandedValueSet {
                    contains,
                    parameters,
                    total,
                    source: Some(server),
                    properties,
                });
            }
        }
        None
    }

    /// Read validate-code displays cached in the `.cache` files, plus loaded
    /// CodeSystems. Builds a per-call scan (small corpus).
    fn cache_lookup_display(&self, system: &str, code: &str) -> Option<String> {
        for cf in &self.cache_files {
            let Some(text) = self.ctx.tree().read(cf) else {
                continue;
            };
            for (req, tag, resp) in parse_cache_blocks(&text) {
                if tag != "v" {
                    continue;
                }
                let Some(req_v) = serde_json::from_str::<Value>(&req).ok() else {
                    continue;
                };
                // request code coding: {"code":{"coding":[{system,code}]}} or {"code":{"code":..},"url":..}
                let (rsys, rcode) = extract_req_code(&req_v);
                if rsys.as_deref() == Some(system) && rcode.as_deref() == Some(code) {
                    if let Some(resp_v) = serde_json::from_str::<Value>(&resp).ok() {
                        if let Some(d) = resp_v.get("display").and_then(|x| x.as_str()) {
                            return Some(d.to_string());
                        }
                    }
                }
            }
        }
        None
    }
}

impl TxCacheSource for FsTxCache<'_> {
    fn expand(&self, _vs_url: &str, vs_json: &Value) -> Option<ExpandedValueSet> {
        // Prefer a cached tx $expand (external systems / non-enumerable);
        // fall back to local internal enumeration.
        if let Some(e) = self.cache_expand(vs_json) {
            return Some(e);
        }
        self.internal_expand(vs_json)
    }

    fn lookup_display(&self, system: &str, code: &str, version: &str) -> Option<String> {
        // Loaded CodeSystem first.
        let canonical = if version.is_empty() {
            system.to_string()
        } else {
            format!("{}|{}", system, version)
        };
        if let Some(cs) = self
            .ctx
            .load_resource(&canonical)
            .or_else(|| self.ctx.load_resource(system))
        {
            if let Some(d) = cs_lookup_display(&cs, code) {
                return Some(d);
            }
        }
        if self.dir.is_some() {
            return self.cache_lookup_display(system, code);
        }
        None
    }
}

fn mk_contains(
    system: &str,
    _version: &str,
    code: &str,
    display: Option<&str>,
    props: &[Value],
) -> Value {
    // The publisher's local expander does NOT stamp a per-code `version` on the
    // contains entries (the version lives in the `used-codesystem` param); so the
    // expansion table shows no Version column. The JSON/XML copy version is read
    // from that param via getVersionForSystem.
    let mut m = serde_json::Map::new();
    m.insert("system".into(), Value::String(system.to_string()));
    m.insert("code".into(), Value::String(code.to_string()));
    if let Some(d) = display {
        m.insert("display".into(), Value::String(d.to_string()));
    }
    if !props.is_empty() {
        m.insert("property".into(), Value::Array(props.to_vec()));
    }
    Value::Object(m)
}

/// Hierarchy properties (parent/child links) are consumed by the expander for
/// nesting, not shown as columns — so they never become expansion properties.
fn is_hierarchy_property(uri: &str) -> bool {
    matches!(
        uri,
        "http://hl7.org/fhir/concept-properties#parent"
            | "http://hl7.org/fhir/concept-properties#child"
            | "http://hl7.org/fhir/concept-properties#partOf"
    )
}

/// The CS `property[]` definitions → the (code, uri) pairs (only those with a
/// uri, e.g. `status` → concept-properties#status). Hierarchy props are skipped.
fn collect_cs_property_defs(cs: &Value, out: &mut std::collections::BTreeMap<String, String>) {
    if let Some(props) = cs.get("property").and_then(|x| x.as_array()) {
        for p in props {
            if let (Some(code), Some(uri)) = (
                p.get("code").and_then(|x| x.as_str()),
                p.get("uri").and_then(|x| x.as_str()),
            ) {
                if is_hierarchy_property(uri) {
                    continue;
                }
                out.entry(code.to_string())
                    .or_insert_with(|| uri.to_string());
            }
        }
    }
}

/// The set of property codes that are hierarchy links in this CS (to drop from
/// the carried concept properties).
fn hierarchy_prop_codes(cs: &Value) -> Vec<String> {
    cs.get("property")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter(|p| {
                    p.get("uri")
                        .and_then(|x| x.as_str())
                        .map(is_hierarchy_property)
                        .unwrap_or(false)
                })
                .filter_map(|p| p.get("code").and_then(|x| x.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// A concept's property[] entries as expansion-contains property values
/// ({code, valueX}). Only scalar-valued props (code/string/boolean/integer).
fn cs_concept_props(cs: &Value, code: &str) -> Vec<Value> {
    fn find<'a>(list: &'a [Value], code: &str) -> Option<&'a Value> {
        for c in list {
            if c.get("code").and_then(|x| x.as_str()) == Some(code) {
                return Some(c);
            }
            if let Some(sub) = c.get("concept").and_then(|x| x.as_array()) {
                if let Some(f) = find(sub, code) {
                    return Some(f);
                }
            }
        }
        None
    }
    let Some(concepts) = cs.get("concept").and_then(|x| x.as_array()) else {
        return Vec::new();
    };
    let Some(c) = find(concepts, code) else {
        return Vec::new();
    };
    let hier = hierarchy_prop_codes(cs);
    c.get("property")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter(|p| keep_concept_prop(p, &hier))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// A concept property to carry into the expansion: not a hierarchy link, and not
/// the suppressed default `status=active`.
fn keep_concept_prop(p: &Value, hier: &[String]) -> bool {
    let code = p.get("code").and_then(|x| x.as_str()).unwrap_or("");
    if hier.iter().any(|h| h == code) {
        return false;
    }
    !(code == "status" && p.get("valueCode").and_then(|x| x.as_str()) == Some("active"))
}

fn contains_any_prop(contains: &[Value], code: &str) -> bool {
    contains.iter().any(|c| {
        c.get("property")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .any(|p| p.get("code").and_then(|x| x.as_str()) == Some(code))
            })
            .unwrap_or(false)
    })
}

fn count_flat(contains: &[Value]) -> usize {
    let mut n = 0;
    for c in contains {
        n += 1;
        if let Some(sub) = c.get("contains").and_then(|x| x.as_array()) {
            n += count_flat(sub);
        }
    }
    n
}

fn enumerate_cs(
    concepts: &[Value],
    system: &str,
    version: &str,
    hier: &[String],
    out: &mut Vec<Value>,
) {
    for c in concepts {
        let code = c.get("code").and_then(|x| x.as_str()).unwrap_or("");
        let display = c.get("display").and_then(|x| x.as_str());
        let props: Vec<Value> = c
            .get("property")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter(|p| keep_concept_prop(p, hier))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        out.push(mk_contains(system, version, code, display, &props));
        if let Some(sub) = c.get("concept").and_then(|x| x.as_array()) {
            enumerate_cs(sub, system, version, hier, out);
        }
    }
}

fn cs_lookup_display(cs: &Value, code: &str) -> Option<String> {
    fn find(list: &[Value], code: &str) -> Option<String> {
        for c in list {
            if c.get("code").and_then(|x| x.as_str()) == Some(code) {
                return c.get("display").and_then(|x| x.as_str()).map(String::from);
            }
            if let Some(sub) = c.get("concept").and_then(|x| x.as_array()) {
                if let Some(f) = find(sub, code) {
                    return Some(f);
                }
            }
        }
        None
    }
    find(cs.get("concept")?.as_array()?, code)
}

/// A stable fingerprint of an include list: for each include, (system, sorted
/// codes). Ignores display/other fields, so a request/VS pair matches by the
/// codes it enumerates.
fn compose_fingerprint(includes: &[Value]) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    for inc in includes {
        let system = inc
            .get("system")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let mut codes: Vec<String> = inc
            .get("concept")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.get("code").and_then(|x| x.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        codes.sort();
        // include filters in the fingerprint so filtered includes match distinctly.
        if let Some(filters) = inc.get("filter").and_then(|x| x.as_array()) {
            for f in filters {
                let p = f.get("property").and_then(|x| x.as_str()).unwrap_or("");
                let op = f.get("op").and_then(|x| x.as_str()).unwrap_or("");
                let v = f.get("value").and_then(|x| x.as_str()).unwrap_or("");
                codes.push(format!("~filter~{}~{}~{}", p, op, v));
            }
            codes.sort();
        }
        out.push((system, codes));
    }
    out.sort();
    out
}

fn extract_req_code(req: &Value) -> (Option<String>, Option<String>) {
    let Some(code) = req.get("code") else {
        return (None, None);
    };
    // form 1: {"code": {"coding": [{system, code}]}}
    if let Some(coding) = code
        .get("coding")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
    {
        return (
            coding
                .get("system")
                .and_then(|x| x.as_str())
                .map(String::from),
            coding
                .get("code")
                .and_then(|x| x.as_str())
                .map(String::from),
        );
    }
    // form 2: {"code": {"system": ..., "code": ...}}  (Coding inline)
    if let Some(sys) = code.get("system").and_then(|x| x.as_str()) {
        return (
            Some(sys.to_string()),
            code.get("code").and_then(|x| x.as_str()).map(String::from),
        );
    }
    // form 3: {"code": {"code": ...}, "system"/"url": ...}  (bare code)
    let sys = req
        .get("system")
        .and_then(|x| x.as_str())
        .or_else(|| req.get("url").and_then(|x| x.as_str()))
        .map(String::from);
    let c = code.get("code").and_then(|x| x.as_str()).map(String::from);
    (sys, c)
}

/// Extract the JSON object that follows `key` in `text` (e.g. `"valueSet"` →
/// the `{...}` after the colon), by brace matching. Handles strings/escapes.
fn extract_json_object(text: &str, key: &str) -> Option<Value> {
    let ki = text.find(key)?;
    let after = &text[ki + key.len()..];
    let colon = after.find(':')?;
    let rest = &after[colon + 1..];
    let start = rest.find('{')?;
    let bytes = rest.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut end = None;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else {
            match c {
                '"' => in_str = true,
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    let obj = &rest[start..end?];
    serde_json::from_str::<Value>(obj).ok()
}

/// Read the bare `"source" : <host>` value from the cache `e:` envelope (the
/// value is unquoted, so we read to the next comma/newline).
fn extract_bare_source(text: &str) -> Option<String> {
    let ki = text.find("\"source\"")?;
    let after = &text[ki + 8..];
    let colon = after.find(':')?;
    let rest = after[colon + 1..].trim_start();
    let end = rest.find([',', '\n', '\r', '}']).unwrap_or(rest.len());
    let val = rest[..end].trim().trim_matches('"');
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}

fn strip_scheme(s: &str) -> String {
    s.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("//")
        .trim_end_matches('/')
        .split('/')
        .next()
        .unwrap_or(s)
        .to_string()
}

/// Parse a `.cache` file into (request, tag, response) blocks. Blocks are
/// separated by a line consisting solely of `-`; each block is
/// `<request>####<tag>: <response>`.
fn parse_cache_blocks(text: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    // Split on the delimiter line of dashes.
    for block in split_on_dash_lines(text) {
        let block = block.trim_matches(['\n', '\r']);
        if block.is_empty() {
            continue;
        }
        let Some(idx) = block.find("####") else {
            continue;
        };
        let req = block[..idx].to_string();
        let after = &block[idx + 4..];
        // after = "<tag>: <response>"
        let Some(colon) = after.find(':') else {
            continue;
        };
        let tag = after[..colon].trim().to_string();
        let resp = after[colon + 1..].trim_start().to_string();
        out.push((req, tag, resp));
    }
    out
}

fn split_on_dash_lines(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut cur = String::new();
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if !trimmed.is_empty() && trimmed.chars().all(|c| c == '-') {
            blocks.push(std::mem::take(&mut cur));
        } else {
            cur.push_str(line);
        }
    }
    blocks.push(cur);
    blocks
}

// keep HashMap import used (reserved for future keyed cache index).
#[allow(dead_code)]
fn _reserved(_: HashMap<String, String>) {}
