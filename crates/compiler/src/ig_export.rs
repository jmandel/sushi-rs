//! ImplementationGuide resource export (`ImplementationGuide-<packageId>.json`).
//!
//! Port of the JSON-resource-building parts of `sushi-ts/src/ig/IGExporter.ts`
//! (`initIG`, `fixDependsOn`, `addResources`/`addPackageResource`,
//! `addConfiguredResources`, `sortResources`, `addConfiguredGroups`,
//! `addConfiguredPageContent`, `updateForR5`/`translateR5PropertiesToR4`). The
//! HTML/page-file generation is intentionally NOT ported — only the in-memory IG
//! resource that is written to disk.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value as J};
use serde_yaml::Value as Y;

use crate::config::Config;
use crate::export::Exported;
use crate::instance_export::IgInstanceMeta;

/// A conformance resource (Profile/Extension/Logical SD, ValueSet, CodeSystem)
/// contributing a `definition.resource` entry.
pub struct ConformanceRes {
    pub reference_key: String,
    pub name: Option<String>,
    pub description: Option<String>,
    /// The resource's actual `name` element (FSH name), used for fishing by name.
    pub fhir_name: Option<String>,
    /// The resource's canonical `url`, returned when normalizing a reference.
    pub url: Option<String>,
}

/// Inputs gathered during `build_project`.
pub struct IgInputs<'a> {
    /// Profiles, extensions, logicals, valueSets, codeSystems — in that push order.
    pub conformance: Vec<ConformanceRes>,
    /// Written instances (usage != Inline).
    pub instances: Vec<&'a IgInstanceMeta>,
    /// url -> effective version, for every LOCAL Profile or Logical SD.
    pub local_profile_logical: HashMap<String, String>,
    /// Whether the project defines any custom Resource (kind=resource) SD.
    pub has_custom_resources: bool,
    pub cache_dir: String,
    /// The IG project root (for disk page scanning).
    pub ig_dir: String,
}

// ---------------------------------------------------------------------------
// YAML helpers.
// ---------------------------------------------------------------------------

fn yget<'a>(v: &'a Y, key: &str) -> Option<&'a Y> {
    v.as_mapping()?.get(&Y::String(key.to_string()))
}

fn ystr(v: &Y) -> Option<String> {
    match v {
        Y::String(s) => Some(s.clone()),
        Y::Bool(b) => Some(b.to_string()),
        Y::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn yget_str(v: &Y, key: &str) -> Option<String> {
    yget(v, key).and_then(ystr)
}

/// `normalizeToArray`: wrap a scalar/map in a single-element vec; pass arrays.
fn norm_array(v: &Y) -> Vec<Y> {
    match v {
        Y::Sequence(s) => s.clone(),
        Y::Null => vec![],
        other => vec![other.clone()],
    }
}

/// Convert a YAML value to JSON, preserving key order and number kinds.
fn yaml_to_json(v: &Y) -> J {
    match v {
        Y::Null => J::Null,
        Y::Bool(b) => J::Bool(*b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                J::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                J::Number(u.into())
            } else if let Some(f) = n.as_f64() {
                serde_json::Number::from_f64(f).map(J::Number).unwrap_or(J::Null)
            } else {
                J::Null
            }
        }
        Y::String(s) => J::String(s.clone()),
        Y::Sequence(seq) => J::Array(seq.iter().map(yaml_to_json).collect()),
        Y::Mapping(map) => {
            let mut o = Map::new();
            for (k, val) in map {
                if let Some(ks) = ystr(k) {
                    o.insert(ks, yaml_to_json(val));
                }
            }
            J::Object(o)
        }
        Y::Tagged(t) => yaml_to_json(&t.value),
    }
}

// ---------------------------------------------------------------------------
// FSH code parsing (jurisdiction etc.).
// ---------------------------------------------------------------------------

/// `parseCodeLexeme`: split `system#code`, handling escaped `#`.
fn parse_code_lexeme(text: &str) -> (Option<String>, String) {
    // find first unescaped '#'
    let bytes = text.as_bytes();
    let mut idx = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            // count preceding backslashes
            let mut bs = 0;
            let mut j = i;
            while j > 0 && bytes[j - 1] == b'\\' {
                bs += 1;
                j -= 1;
            }
            if bs % 2 == 0 {
                idx = Some(i);
                break;
            }
        }
        i += 1;
    }
    match idx {
        None => {
            let code = if let Some(stripped) = text.strip_prefix('#') {
                stripped.to_string()
            } else {
                String::new()
            };
            (None, code)
        }
        Some(p) => {
            let system = text[..p].replace("\\\\", "\\").replace("\\#", "#");
            let mut code = text[p + 1..].to_string();
            if code.starts_with('"') && code.ends_with('"') && code.len() >= 2 {
                code = code[1..code.len() - 1]
                    .replace("\\\\", "\\")
                    .replace("\\\"", "\"");
            }
            let sys = if system.is_empty() { None } else { Some(system) };
            (sys, code)
        }
    }
}

/// `parseFshCode`: like parse_code_lexeme but also extracts a trailing `"display"`.
fn parse_fsh_code(text: &str) -> Option<(Option<String>, String, Option<String>)> {
    // match ^(.*\S)(\s+"(...)")$
    let display;
    let core;
    if let Some(qpos) = find_display_split(text) {
        core = text[..qpos].trim_end().to_string();
        let disp = &text[qpos..];
        let disp = disp.trim_start();
        let inner = &disp[1..disp.len() - 1];
        display = Some(inner.replace("\\\"", "\""));
    } else {
        core = text.to_string();
        display = None;
    }
    let (system, code) = parse_code_lexeme(&core);
    if system.is_none() && code.is_empty() {
        return None;
    }
    Some((system, code, display))
}

/// Find the byte index where a trailing whitespace + `"display"` begins, or None.
fn find_display_split(text: &str) -> Option<usize> {
    let t = text.trim_end();
    if !t.ends_with('"') {
        return None;
    }
    // find the opening quote: scan from the end for an unescaped `"` preceded by whitespace.
    let bytes = t.as_bytes();
    let mut i = t.len() - 1; // closing quote
    // walk back to matching opening quote
    let mut j = i;
    while j > 0 {
        j -= 1;
        if bytes[j] == b'"' {
            // count escaping backslashes
            let mut bs = 0;
            let mut k = j;
            while k > 0 && bytes[k - 1] == b'\\' {
                bs += 1;
                k -= 1;
            }
            if bs % 2 == 0 {
                // opening quote at j; require whitespace before it and non-ws before that
                if j == 0 {
                    return None;
                }
                let before = &t[..j];
                if before.ends_with(char::is_whitespace) && !before.trim_end().is_empty() {
                    return Some(j);
                }
                return None;
            }
        }
    }
    let _ = i;
    i = 0;
    let _ = i;
    None
}

/// `parseCodeableConcept(string)` → `{coding:[{code,system?,display?}]}`.
fn parse_codeable_concept(text: &str) -> Option<J> {
    let (system, code, display) = parse_fsh_code(text)?;
    let mut coding = Map::new();
    coding.insert("code".into(), J::String(code));
    if let Some(s) = system {
        coding.insert("system".into(), J::String(s));
    }
    if let Some(d) = display {
        coding.insert("display".into(), J::String(d));
    }
    let mut cc = Map::new();
    cc.insert("coding".into(), J::Array(vec![J::Object(coding)]));
    Some(J::Object(cc))
}

// ---------------------------------------------------------------------------
// Main entry.
// ---------------------------------------------------------------------------

/// Build the ImplementationGuide resource. Returns the exported file (filename +
/// ordered JSON body), or `None` if the config lacks the data to build one.
pub fn export_ig(cfg_yaml: &Y, cfg: &Config, inputs: &IgInputs) -> Option<Exported> {
    let id = yget_str(cfg_yaml, "id")?;
    let canonical = cfg.canonical.clone();
    let fhir_version = cfg.fhir_version();
    let is_r4 = fhir_version
        .as_deref()
        .map(is_r4_version)
        .unwrap_or(true);

    let mut ig: Map<String, J> = Map::new();
    ig.insert("resourceType".into(), J::String("ImplementationGuide".into()));
    ig.insert("id".into(), J::String(id.clone()));

    // Optional passthrough metadata keys (in literal order). Only `extension` is
    // commonly present in our corpus, but keep the others for completeness.
    insert_passthrough(&mut ig, cfg_yaml, "meta");
    insert_passthrough(&mut ig, cfg_yaml, "implicitRules");
    insert_passthrough(&mut ig, cfg_yaml, "language");
    insert_passthrough(&mut ig, cfg_yaml, "text");
    insert_passthrough(&mut ig, cfg_yaml, "contained");
    insert_passthrough(&mut ig, cfg_yaml, "extension");
    insert_passthrough(&mut ig, cfg_yaml, "modifierExtension");

    // url
    let url = yget_str(cfg_yaml, "url")
        .unwrap_or_else(|| format!("{canonical}/ImplementationGuide/{id}"));
    ig.insert("url".into(), J::String(url));

    // version
    if let Some(v) = yget(cfg_yaml, "version").and_then(ystr) {
        ig.insert("version".into(), J::String(v));
    }
    // name (strip non-alphanumeric/underscore)
    if let Some(name) = yget_str(cfg_yaml, "name") {
        let cleaned: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        ig.insert("name".into(), J::String(cleaned));
    }
    if let Some(t) = yget_str(cfg_yaml, "title") {
        ig.insert("title".into(), J::String(t));
    }
    // status (default draft)
    ig.insert("status".into(), J::String(cfg.status().to_string()));
    if let Some(exp) = yget(cfg_yaml, "experimental") {
        if let Y::Bool(b) = exp {
            ig.insert("experimental".into(), J::Bool(*b));
        }
    }
    insert_passthrough(&mut ig, cfg_yaml, "date");

    // publisher (first publisher's name)
    let publishers = yget(cfg_yaml, "publisher").map(norm_array).unwrap_or_default();
    if let Some(first) = publishers.first() {
        if let Some(name) = yget_str(first, "name").or_else(|| ystr(first)) {
            ig.insert("publisher".into(), J::String(name));
        }
    }
    // contact
    if let Some(contact) = build_contact(cfg_yaml, &publishers) {
        ig.insert("contact".into(), contact);
    }
    if let Some(d) = yget_str(cfg_yaml, "description") {
        ig.insert("description".into(), J::String(d));
    }
    insert_passthrough(&mut ig, cfg_yaml, "useContext");
    // jurisdiction
    if let Some(j) = build_jurisdiction(cfg_yaml) {
        ig.insert("jurisdiction".into(), j);
    }
    insert_passthrough(&mut ig, cfg_yaml, "copyright");
    // packageId
    let package_id = yget_str(cfg_yaml, "packageId").unwrap_or_else(|| id.clone());
    ig.insert("packageId".into(), J::String(package_id));
    if let Some(l) = yget_str(cfg_yaml, "license") {
        ig.insert("license".into(), J::String(l));
    }
    // fhirVersion (array)
    if let Some(fv) = yget(cfg_yaml, "fhirVersion") {
        let arr: Vec<J> = norm_array(fv)
            .iter()
            .filter_map(ystr)
            .map(J::String)
            .collect();
        ig.insert("fhirVersion".into(), J::Array(arr));
    }

    // dependsOn
    let depends = build_depends_on(cfg_yaml, is_r4, &inputs.cache_dir);
    if let Some(d) = depends {
        if !d.is_empty() {
            ig.insert("dependsOn".into(), J::Array(d));
        }
    }

    // global (passthrough, deleted if empty — we just include if non-empty array)
    if let Some(g) = yget(cfg_yaml, "global") {
        let arr = build_global(g);
        if !arr.is_empty() {
            ig.insert("global".into(), J::Array(arr));
        }
    }

    // definition
    let definition = build_definition(cfg_yaml, cfg, inputs, is_r4, &canonical);
    ig.insert("definition".into(), definition);

    // R5-only top-level additions (after definition).
    if !is_r4 {
        if let Some(cl) = yget_str(cfg_yaml, "copyrightLabel") {
            ig.insert(
                "copyrightLabel".into(),
                J::String(cl),
            );
        }
    }

    let filename = format!("ImplementationGuide-{id}.json");
    Some(Exported {
        filename,
        body: J::Object(ig),
    })
}

fn insert_passthrough(ig: &mut Map<String, J>, cfg_yaml: &Y, key: &str) {
    if let Some(v) = yget(cfg_yaml, key) {
        if !matches!(v, Y::Null) {
            ig.insert(key.to_string(), yaml_to_json(v));
        }
    }
}

/// `^R4B?$` FHIR version check (after `getFHIRVersionInfo`).
fn is_r4_version(v: &str) -> bool {
    v.starts_with("4.0") || v.starts_with("4.1") || v.starts_with("4.3") || v == "4.0.1"
}

// ---------------------------------------------------------------------------
// contact / jurisdiction.
// ---------------------------------------------------------------------------

fn build_contact(cfg_yaml: &Y, publishers: &[Y]) -> Option<J> {
    let mut contacts: Vec<J> = Vec::new();
    for (i, p) in publishers.iter().enumerate() {
        let name = yget_str(p, "name").or_else(|| ystr(p));
        let url = yget_str(p, "url");
        let email = yget_str(p, "email");
        let mut contact = Map::new();
        if let Some(n) = &name {
            contact.insert("name".into(), J::String(n.clone()));
        }
        if url.is_some() || email.is_some() {
            let mut tel = Vec::new();
            if let Some(u) = url {
                let mut t = Map::new();
                t.insert("system".into(), J::String("url".into()));
                t.insert("value".into(), J::String(u));
                tel.push(J::Object(t));
            }
            if let Some(e) = email {
                let mut t = Map::new();
                t.insert("system".into(), J::String("email".into()));
                t.insert("value".into(), J::String(e));
                tel.push(J::Object(t));
            }
            contact.insert("telecom".into(), J::Array(tel));
        } else if i == 0 {
            // first publisher with no extra contact detail → skip
            continue;
        }
        contacts.push(J::Object(contact));
    }
    if let Some(yc) = yget(cfg_yaml, "contact") {
        for c in norm_array(yc) {
            let mut contact = yaml_to_json(&c);
            // normalize telecom (spread, keeping authoring order); empty telecom dropped
            if let Some(obj) = contact.as_object_mut() {
                if let Some(tel) = obj.get("telecom").cloned() {
                    let arr: Vec<J> = match tel {
                        J::Array(a) => a,
                        other => vec![other],
                    };
                    if arr.is_empty() {
                        obj.remove("telecom");
                    } else {
                        obj.insert("telecom".into(), J::Array(arr));
                    }
                }
            }
            contacts.push(contact);
        }
    }
    if contacts.is_empty() {
        None
    } else {
        Some(J::Array(contacts))
    }
}

fn build_jurisdiction(cfg_yaml: &Y) -> Option<J> {
    let j = yget(cfg_yaml, "jurisdiction")?;
    let mut out = Vec::new();
    for item in norm_array(j) {
        match item {
            Y::String(s) => {
                if let Some(cc) = parse_codeable_concept(&s) {
                    out.push(cc);
                }
            }
            other => out.push(yaml_to_json(&other)),
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(J::Array(out))
    }
}

fn build_global(g: &Y) -> Vec<J> {
    let mut out = Vec::new();
    if let Y::Mapping(map) = g {
        for (ty, profiles) in map {
            let Some(tys) = ystr(ty) else { continue };
            for prof in norm_array(profiles) {
                if let Some(ps) = ystr(&prof) {
                    let mut o = Map::new();
                    o.insert("type".into(), J::String(tys.clone()));
                    o.insert("profile".into(), J::String(ps));
                    out.push(J::Object(o));
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// dependsOn.
// ---------------------------------------------------------------------------

fn build_depends_on(cfg_yaml: &Y, is_r4: bool, cache_dir: &str) -> Option<Vec<J>> {
    let deps = yget(cfg_yaml, "dependencies")?;
    let Y::Mapping(map) = deps else { return None };
    let mut out = Vec::new();
    for (pkg_key, val) in map {
        let Some(package_id) = ystr(pkg_key) else { continue };
        let mut package_id = if package_id.chars().any(|c| c.is_ascii_uppercase()) {
            package_id.to_lowercase()
        } else {
            package_id
        };

        // Parse config entry preserving key order: id, packageId, uri, version, (reason/extension).
        let mut entry: Vec<(String, J)> = Vec::new();
        let mut reason: Option<String> = None;
        let mut version: Option<String> = None;
        let mut uri: Option<String> = None;
        let mut id: Option<String> = None;
        let mut explicit_ext: Option<J> = None;

        match val {
            Y::String(_) | Y::Number(_) => {
                version = ystr(val);
                entry.push(("packageId".into(), J::String(package_id.clone())));
                if let Some(v) = &version {
                    entry.push(("version".into(), J::String(v.clone())));
                }
            }
            Y::Mapping(_) => {
                id = yget_str(val, "id");
                uri = yget_str(val, "uri");
                version = yget(val, "version").and_then(ystr);
                reason = yget_str(val, "reason");
                explicit_ext = yget(val, "extension").map(yaml_to_json);
                // removeUndefinedValues: build in order id, packageId, uri, version, [extension]
                if let Some(i) = &id {
                    entry.push(("id".into(), J::String(i.clone())));
                }
                entry.push(("packageId".into(), J::String(package_id.clone())));
                if let Some(u) = &uri {
                    entry.push(("uri".into(), J::String(u.clone())));
                }
                if let Some(v) = &version {
                    entry.push(("version".into(), J::String(v.clone())));
                }
                // reason is NOT added here (it's deleted in fixDependsOn); explicit extension kept
                if let Some(e) = &explicit_ext {
                    entry.push(("extension".into(), e.clone()));
                }
            }
            _ => {
                entry.push(("packageId".into(), J::String(package_id.clone())));
            }
        }

        // fixCrossVersionDependencies (Processing.ts:542, applied for the IG by
        // IGExporter.ts:230): replace an old-style cross-version extensions
        // dependency (e.g. `hl7.fhir.extensions.r5#4.0.1`) with the official xver
        // package (`hl7.fhir.uv.xver-r5.r4`). Stock clones the dep and overrides
        // packageId/version/uri; `version` becomes `latest` (later resolved to the
        // installed version by fixDependsOn), `uri` is set explicitly, and `id` is
        // regenerated from the new packageId (or kept if the config gave one).
        if let Some((xver_pkg, xver_uri)) = fix_cross_version_dep(&package_id, version.as_deref()) {
            package_id = xver_pkg.clone();
            version = Some("latest".into());
            uri = Some(xver_uri.clone());
            entry.clear();
            if let Some(i) = &id {
                entry.push(("id".into(), J::String(i.clone())));
            }
            entry.push(("packageId".into(), J::String(xver_pkg)));
            entry.push(("version".into(), J::String("latest".into())));
            entry.push(("uri".into(), J::String(xver_uri)));
            if let Some(e) = &explicit_ext {
                entry.push(("extension".into(), e.clone()));
            }
        }

        // fixDependsOn: version required, else drop.
        let Some(raw_version) = version.clone() else {
            continue;
        };

        // Resolve the version used for the URI lookup (fixDependsOn,
        // IGExporter.ts:289-315): `latest` -> a matching installed version (also
        // mutating the emitted version, as stock does); `M.m.x` -> maxSatisfying
        // over installed versions (emitted version kept as the raw config value).
        let resolved_version =
            resolve_depends_on_version(cache_dir, &package_id, &raw_version, &mut entry);

        // uri: resolve if missing.
        if uri.is_none() {
            let resolved = find_dependency_ig_url(cache_dir, &package_id, &resolved_version)
                .or_else(|| find_package_canonical(cache_dir, &package_id, &resolved_version))
                .unwrap_or_else(|| {
                    format!("http://fhir.org/packages/{package_id}/ImplementationGuide/{package_id}")
                });
            entry.push(("uri".into(), J::String(resolved)));
        }

        // id: generate if missing.
        if id.is_none() {
            let dep_id = package_id.clone();
            let first = dep_id.chars().next().unwrap_or(' ');
            let gen = if first.is_ascii_alphabetic() {
                dep_id.replace(['.', '-'], "_")
            } else {
                format!("id_{}", dep_id.replace(['.', '-'], "_"))
            };
            entry.push(("id".into(), J::String(gen)));
        }

        // reason → extension (R4) / reason field (R5), appended last.
        if let Some(r) = &reason {
            if is_r4 {
                let ext = J::Array(vec![{
                    let mut m = Map::new();
                    m.insert(
                        "url".into(),
                        J::String(
                            "http://hl7.org/fhir/5.0/StructureDefinition/extension-ImplementationGuide.dependsOn.reason"
                                .into(),
                        ),
                    );
                    m.insert("valueMarkdown".into(), J::String(r.clone()));
                    J::Object(m)
                }]);
                // merge with any explicit extension already present
                merge_or_set_extension(&mut entry, ext);
            } else {
                entry.push(("reason".into(), J::String(r.clone())));
            }
        }

        let mut obj = Map::new();
        for (k, v) in entry {
            obj.insert(k, v);
        }
        out.push(J::Object(obj));
    }
    Some(out)
}

fn merge_or_set_extension(entry: &mut Vec<(String, J)>, ext: J) {
    if let Some((_, existing)) = entry.iter_mut().find(|(k, _)| k == "extension") {
        if let (Some(ea), J::Array(na)) = (existing.as_array().cloned(), &ext) {
            let mut combined = ea;
            combined.extend(na.clone());
            *existing = J::Array(combined);
            return;
        }
    }
    entry.push(("extension".into(), ext));
}

/// Enumerate installed versions of `package_id` by scanning the cache for
/// `{package_id}#<version>` directories (the FHIR cache layout). Mirrors the
/// set of cached IGs stock filters by `packageId` in fixDependsOn.
fn installed_versions(cache_dir: &str, package_id: &str) -> Vec<String> {
    let prefix = format!("{package_id}#");
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(cache_dir) else { return out };
    for entry in rd.flatten() {
        if let Some(name) = entry.file_name().to_str() {
            if let Some(ver) = name.strip_prefix(&prefix) {
                if !ver.is_empty() {
                    out.push(ver.to_string());
                }
            }
        }
    }
    out
}

/// Compare two dotted numeric versions (zero-padded to equal length).
fn version_cmp(a: &[u64], b: &[u64]) -> std::cmp::Ordering {
    let n = a.len().max(b.len());
    for i in 0..n {
        let av = a.get(i).copied().unwrap_or(0);
        let bv = b.get(i).copied().unwrap_or(0);
        match av.cmp(&bv) {
            std::cmp::Ordering::Equal => {}
            other => return other,
        }
    }
    std::cmp::Ordering::Equal
}

/// `maxSatisfying(versions, range)` for an x-range like `4.0.x` or `4.x`.
/// The numeric components before the first `x`/`*` are fixed; the matching
/// version with the greatest remaining components is returned. Prerelease
/// versions (containing `-`) are excluded, matching node-semver defaults.
fn max_satisfying_x(installed: &[String], range: &str) -> Option<String> {
    let mut fixed: Vec<u64> = Vec::new();
    for p in range.split('.') {
        if p == "x" || p == "X" || p == "*" {
            break;
        }
        fixed.push(p.parse::<u64>().ok()?);
    }
    let mut best: Option<(Vec<u64>, String)> = None;
    for v in installed {
        if v.contains('-') {
            continue;
        }
        let Ok(vp) = v
            .split('.')
            .map(|s| s.parse::<u64>())
            .collect::<Result<Vec<u64>, _>>()
        else {
            continue;
        };
        if vp.len() < fixed.len() {
            continue;
        }
        if !fixed.iter().zip(&vp).all(|(a, b)| a == b) {
            continue;
        }
        let better = match &best {
            Some((bv, _)) => version_cmp(&vp, bv) == std::cmp::Ordering::Greater,
            None => true,
        };
        if better {
            best = Some((vp, v.clone()));
        }
    }
    best.map(|(_, s)| s)
}

/// Port of `fixCrossVersionDependencies` (Processing.ts:542) for a single dep.
///
/// Matches a legacy cross-version extensions package id (`^hl7.fhir.extensions.rNb?$`)
/// and rewrites it to the official xver package. `source` is the release suffix of
/// the legacy id (`r5` from `hl7.fhir.extensions.r5`); `target` is the release the
/// declared version belongs to (`4.0.1` -> `r4`), giving `hl7.fhir.uv.xver-r5.r4`
/// and `http://hl7.org/fhir/uv/xver/ImplementationGuide/hl7.fhir.uv.xver-r5.r4`.
/// Returns `None` (leaving the dep untouched) when the id doesn't match or the
/// version's release can't be determined.
fn fix_cross_version_dep(package_id: &str, version: Option<&str>) -> Option<(String, String)> {
    let source = package_id.strip_prefix("hl7.fhir.extensions.")?;
    if !is_release_suffix(source) {
        return None;
    }
    let target = fhir_version_release_suffix(version?)?;
    let pkg = format!("hl7.fhir.uv.xver-{source}.{target}");
    let uri = format!("http://hl7.org/fhir/uv/xver/ImplementationGuide/{pkg}");
    Some((pkg, uri))
}

/// A FHIR release suffix: `r` + digits + optional trailing `b` (e.g. `r4`, `r4b`, `r5`).
fn is_release_suffix(s: &str) -> bool {
    let Some(digits) = s.strip_prefix('r') else {
        return false;
    };
    let digits = digits.strip_suffix('b').unwrap_or(digits);
    !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
}

/// Lowercased release suffix for a FHIR version string: the `name` from
/// `getFHIRVersionInfo` (FHIRVersionUtils.ts) with `D?STU` -> `r`, lowercased
/// (so `STU3` -> `r3`, `DSTU2` -> `r2`). Returns `None` for the catch-all `??`.
fn fhir_version_release_suffix(version: &str) -> Option<String> {
    let name = fhir_version_name(version)?;
    Some(name.replace("DSTU", "r").replace("STU", "r").to_lowercase())
}

/// Port of the `name` lookup in `getFHIRVersionInfo` (FHIRVersionUtils.ts): the
/// FHIR release name for a version string. Returns `None` for the `??` catch-all.
fn fhir_version_name(version: &str) -> Option<&'static str> {
    let parts: Vec<&str> = version.splitn(3, '.').collect();
    let major = parts.first().copied().unwrap_or("");
    let minor = parts.get(1).copied().unwrap_or("");
    let has_pre = version.contains('-');
    // Mirrors the ordered VERSIONS table; first match wins.
    match major {
        "4" => match minor {
            "0" => Some("R4"),
            "3" => Some("R4B"),
            "1" => Some("R4B"), // 4.1.x pre-release
            _ => Some("R5"),    // other 4.x are R5 pre-releases
        },
        "5" => Some("R5"),
        "6" => Some("R6"),
        "3" => {
            if minor == "0" && !has_pre {
                Some("STU3")
            } else if minor == "0" {
                Some("STU3")
            } else {
                Some("R4") // 3.x (non-0 minor) maps to R4 in the table
            }
        }
        "1" => {
            if minor == "0" {
                Some("DSTU2")
            } else {
                Some("STU3")
            }
        }
        "dev" | "current" => Some("R5"),
        _ if version.starts_with("dev") || version.starts_with("current") => Some("R5"),
        _ => None,
    }
}

/// Resolve the version used for the dependency URI lookup (fixDependsOn,
/// IGExporter.ts:289-315). For `latest`, pick an installed version and mutate
/// the emitted `version` field (stock sets `dependsOn.version`); the
/// single-version-in-scope assumption is resolved to the greatest installed
/// version. For an x-range, return `maxSatisfying` while keeping the emitted
/// version as the raw config value. Otherwise return the raw version.
fn resolve_depends_on_version(
    cache_dir: &str,
    package_id: &str,
    raw_version: &str,
    entry: &mut [(String, J)],
) -> String {
    if raw_version == "latest" {
        let installed = installed_versions(cache_dir, package_id);
        let numeric: Vec<String> = installed.into_iter().filter(|v| !v.contains('-')).collect();
        if let Some(v) = numeric
            .iter()
            .filter_map(|v| {
                v.split('.')
                    .map(|s| s.parse::<u64>())
                    .collect::<Result<Vec<u64>, _>>()
                    .ok()
                    .map(|p| (p, v.clone()))
            })
            .max_by(|a, b| version_cmp(&a.0, &b.0))
            .map(|(_, s)| s)
        {
            if let Some((_, val)) = entry.iter_mut().find(|(k, _)| k == "version") {
                *val = J::String(v.clone());
            }
            return v;
        }
        return raw_version.to_string();
    }
    if raw_version.ends_with(".x") {
        let installed = installed_versions(cache_dir, package_id);
        if let Some(v) = max_satisfying_x(&installed, raw_version) {
            return v;
        }
    }
    raw_version.to_string()
}

/// Read a dependency package's ImplementationGuide `url`.
///
/// FPL loads IG resources by scanning package JSON files, so packages with an
/// empty `.index.json` can still contribute an IG URL. Use package_store's shared
/// package-resource listing helper so this path follows the same index-vs-scan
/// rules as package fishing.
fn find_dependency_ig_url(cache_dir: &str, package_id: &str, version: &str) -> Option<String> {
    let package_dir = Path::new(cache_dir)
        .join(format!("{package_id}#{version}"))
        .join("package");
    for record in package_store::package_resource_entries(&package_dir) {
        let entry = record.entry;
        if entry.resource_type.as_deref() != Some("ImplementationGuide") {
            continue;
        }
        let package_matches = entry
            .package_id
            .as_deref()
            .map(|id| id == package_id)
            .unwrap_or(true);
        let version_matches = entry
            .version
            .as_deref()
            .map(|v| v == version || version == "current" || version == "dev")
            .unwrap_or(true);
        if package_matches && version_matches {
            return entry.url;
        }
    }
    None
}

/// Fallback: the dependency package.json `canonical`.
fn find_package_canonical(cache_dir: &str, package_id: &str, version: &str) -> Option<String> {
    let pj = Path::new(cache_dir)
        .join(format!("{package_id}#{version}"))
        .join("package")
        .join("package.json");
    let bytes = std::fs::read(&pj).ok()?;
    let json: J = serde_json::from_slice(&bytes).ok()?;
    json.get("canonical")
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// normalizeResourceReference (IGExporter.ts:1375-1399).
// ---------------------------------------------------------------------------

/// Port of `normalizeResourceReference`. If `name` is already a relative URL or
/// canonical (contains `/` or `:`) it is returned unchanged. Otherwise we fish
/// the local conformance resources by name/id/url and replace with the relative
/// reference (`Type/id`) when `use_relative`, else the canonical `url`. Falls
/// back to the original string if nothing is found.
fn normalize_resource_reference(
    name: &str,
    use_relative: bool,
    conformance: &[ConformanceRes],
) -> String {
    if name.contains('/') || name.contains(':') {
        return name.to_string();
    }
    for c in conformance {
        let id = c.reference_key.split_once('/').map(|(_, id)| id);
        let matches = c.fhir_name.as_deref() == Some(name)
            || id == Some(name)
            || c.url.as_deref() == Some(name);
        if matches {
            if use_relative {
                // reference_key is always `ResourceType/id`.
                return c.reference_key.clone();
            } else if let Some(url) = &c.url {
                return url.clone();
            }
        }
    }
    name.to_string()
}

// ---------------------------------------------------------------------------
// definition.
// ---------------------------------------------------------------------------

fn build_definition(
    cfg_yaml: &Y,
    cfg: &Config,
    inputs: &IgInputs,
    is_r4: bool,
    canonical: &str,
) -> J {
    let mut def: Map<String, J> = Map::new();

    // definition.extension (only the `extension` subkey under `definition:`).
    if let Some(d) = yget(cfg_yaml, "definition") {
        if let Some(ext) = yget(d, "extension") {
            def.insert("extension".into(), yaml_to_json(ext));
        }
    }

    // Build resources + grouping.
    let mut config_resources = parse_config_resources(cfg_yaml);
    // normalizeResourceReferences: config.resources[].exampleCanonical (a bare
    // profile name) is replaced with its canonical url (useRelative=false).
    for cr in &mut config_resources {
        if let Some(ec) = &cr.example_canonical {
            cr.example_canonical =
                Some(normalize_resource_reference(ec, false, &inputs.conformance));
        }
    }
    let groups = parse_groups(cfg_yaml);
    let (resources, grouping) =
        build_resources(cfg_yaml, inputs, &config_resources, &groups, is_r4, cfg);
    if let Some(g) = grouping {
        if !g.is_empty() {
            def.insert("grouping".into(), J::Array(g));
        }
    }
    def.insert("resource".into(), J::Array(resources));

    // page
    let page = build_page(cfg_yaml, is_r4, &inputs.ig_dir);
    def.insert("page".into(), page);

    // parameter
    let parameters = build_parameters(cfg_yaml, cfg, canonical, is_r4, inputs.has_custom_resources);
    def.insert("parameter".into(), J::Array(parameters));

    // template (passthrough if present)
    if let Some(t) = yget(cfg_yaml, "templates") {
        let arr = yaml_to_json(t);
        if arr.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            def.insert("template".into(), arr);
        }
    }

    J::Object(def)
}

// ---- config.resources -----------------------------------------------------

struct ConfigResource {
    reference: String,
    omit: bool,
    /// ordered (key, value) pairs of `details` (excluding reference), then profile/fhirVersion.
    ordered: Vec<(String, J)>,
    name: Option<String>,
    description: Option<String>,
    grouping_id: Option<String>,
    example_canonical: Option<String>,
    example_boolean: Option<bool>,
    is_example: Option<bool>,
    profile: Option<Vec<String>>,
    fhir_version: Option<Vec<String>>,
    extension: Option<J>,
}

fn parse_config_resources(cfg_yaml: &Y) -> Vec<ConfigResource> {
    let mut out = Vec::new();
    let Some(Y::Mapping(map)) = yget(cfg_yaml, "resources") else {
        return out;
    };
    for (k, details) in map {
        let Some(reference) = ystr(k) else { continue };
        if matches!(details, Y::String(s) if s == "omit" || s == "#omit") {
            out.push(ConfigResource {
                reference,
                omit: true,
                ordered: vec![],
                name: None,
                description: None,
                grouping_id: None,
                example_canonical: None,
                example_boolean: None,
                is_example: None,
                profile: None,
                fhir_version: None,
                extension: None,
            });
            continue;
        }
        let mut ordered: Vec<(String, J)> = Vec::new();
        let mut name = None;
        let mut description = None;
        let mut grouping_id = None;
        let mut example_canonical = None;
        let mut example_boolean = None;
        let mut is_example = None;
        let mut profile = None;
        let mut fhir_version = None;
        let mut extension = None;
        if let Y::Mapping(dm) = details {
            for (dk, dv) in dm {
                let Some(key) = ystr(dk) else { continue };
                match key.as_str() {
                    "name" => name = ystr(dv),
                    "description" => description = ystr(dv),
                    "groupingId" => grouping_id = ystr(dv),
                    "exampleCanonical" => example_canonical = ystr(dv),
                    "exampleBoolean" => {
                        if let Y::Bool(b) = dv {
                            example_boolean = Some(*b);
                        }
                    }
                    "isExample" => {
                        if let Y::Bool(b) = dv {
                            is_example = Some(*b);
                        }
                    }
                    "profile" => {
                        profile = Some(norm_array(dv).iter().filter_map(ystr).collect());
                    }
                    "fhirVersion" => {
                        fhir_version = Some(norm_array(dv).iter().filter_map(ystr).collect());
                    }
                    "extension" => extension = Some(yaml_to_json(dv)),
                    _ => {}
                }
                // ordered spread (for config-only verbatim push)
                ordered.push((key, yaml_to_json(dv)));
            }
        }
        out.push(ConfigResource {
            reference,
            omit: false,
            ordered,
            name,
            description,
            grouping_id,
            example_canonical,
            example_boolean,
            is_example,
            profile,
            fhir_version,
            extension,
        });
    }
    out
}

fn find_config_resource<'a>(
    list: &'a [ConfigResource],
    reference: &str,
) -> Option<&'a ConfigResource> {
    list.iter().find(|c| c.reference == reference)
}

// ---- groups ---------------------------------------------------------------

struct Group {
    id: String,
    name: String,
    description: Option<String>,
    resources: Vec<String>,
}

fn parse_groups(cfg_yaml: &Y) -> Vec<Group> {
    let mut out = Vec::new();
    let Some(Y::Mapping(map)) = yget(cfg_yaml, "groups") else {
        return out;
    };
    for (k, v) in map {
        let Some(id) = ystr(k) else { continue };
        let name = yget_str(v, "name").unwrap_or_else(|| id.clone());
        let description = yget_str(v, "description");
        let resources = yget(v, "resources")
            .map(|r| norm_array(r).iter().filter_map(ystr).collect())
            .unwrap_or_default();
        out.push(Group {
            id,
            name,
            description,
            resources,
        });
    }
    out
}

// ---- resource[] + grouping[] ----------------------------------------------

/// An IG resource entry under construction (preserves key insertion order).
struct ResEntry {
    reference_key: String,
    pairs: Vec<(String, J)>,
    /// sort name (name ?? reference)
    sort_name: String,
}

fn build_resources(
    cfg_yaml: &Y,
    inputs: &IgInputs,
    config_resources: &[ConfigResource],
    groups: &[Group],
    is_r4: bool,
    cfg: &Config,
) -> (Vec<J>, Option<Vec<J>>) {
    let mut entries: Vec<ResEntry> = Vec::new();
    // grouping accumulator (ordered, dedup by id)
    let mut grouping: Vec<(String, String, Option<String>)> = Vec::new();
    let mut add_group = |id: &str, name: Option<&str>, description: Option<&str>| {
        let nm = name.unwrap_or(id).to_string();
        if let Some(existing) = grouping.iter_mut().find(|(gid, _, _)| gid == id) {
            existing.1 = nm;
            if let Some(d) = description {
                if !d.is_empty() {
                    existing.2 = Some(d.to_string());
                }
            }
        } else {
            grouping.push((id.to_string(), nm, description.map(str::to_string)));
        }
    };

    // 1. addResources: conformance then instances.
    for c in &inputs.conformance {
        let cr = find_config_resource(config_resources, &c.reference_key);
        if cr.map(|c| c.omit).unwrap_or(false) {
            continue;
        }
        let entry = make_package_resource_conformance(c, cr, &mut add_group);
        entries.push(entry);
    }
    for inst in &inputs.instances {
        let cr = find_config_resource(config_resources, &inst.reference_key);
        if cr.map(|c| c.omit).unwrap_or(false) {
            continue;
        }
        let entry =
            make_package_resource_instance(inst, cr, inputs, cfg, &mut add_group);
        entries.push(entry);
    }

    // 1b. addPredefinedResources (input/{profiles,resources,examples,...}).
    add_predefined_resources(
        &mut entries,
        cfg_yaml,
        inputs,
        config_resources,
        cfg,
        is_r4,
        &mut add_group,
    );

    // 2. addConfiguredResources: config-only entries not already present.
    for c in config_resources {
        if c.omit {
            continue;
        }
        if entries.iter().any(|e| e.reference_key == c.reference) {
            continue;
        }
        entries.push(make_config_only_resource(c));
    }

    // normalizeResourceReferences: group resources that are bare names resolve to
    // `Type/id` (fished against the package). Resolve against the built entries.
    let mut id_to_ref: HashMap<String, String> = HashMap::new();
    for e in &entries {
        if let Some((_, id)) = e.reference_key.split_once('/') {
            id_to_ref.entry(id.to_string()).or_insert_with(|| e.reference_key.clone());
        }
    }
    let groups: Vec<Group> = groups
        .iter()
        .map(|g| Group {
            id: g.id.clone(),
            name: g.name.clone(),
            description: g.description.clone(),
            resources: g
                .resources
                .iter()
                .map(|r| {
                    if r.contains('/') || r.contains(':') {
                        r.clone()
                    } else {
                        id_to_ref.get(r).cloned().unwrap_or_else(|| r.clone())
                    }
                })
                .collect(),
        })
        .collect();
    let groups = &groups[..];

    // 3. sortResources.
    sort_resources(&mut entries, config_resources, groups);

    // 4. addConfiguredGroups.
    for g in groups {
        add_group(&g.id, Some(&g.name), g.description.as_deref());
        for rref in &g.resources {
            if let Some(e) = entries.iter_mut().find(|e| &e.reference_key == rref) {
                set_or_append(&mut e.pairs, "groupingId", J::String(g.id.clone()));
            }
        }
    }

    // 5. R5/R4 transforms on each resource entry.
    for e in &mut entries {
        transform_resource_entry(&mut e.pairs, is_r4);
    }

    let resources: Vec<J> = entries
        .into_iter()
        .map(|e| {
            let mut o = Map::new();
            for (k, v) in e.pairs {
                o.insert(k, v);
            }
            J::Object(o)
        })
        .collect();

    let grouping_out = if grouping.is_empty() {
        None
    } else {
        Some(
            grouping
                .into_iter()
                .map(|(id, name, desc)| {
                    let mut o = Map::new();
                    o.insert("id".into(), J::String(id));
                    o.insert("name".into(), J::String(name));
                    if let Some(d) = desc {
                        o.insert("description".into(), J::String(d));
                    }
                    J::Object(o)
                })
                .collect(),
        )
    };
    (resources, grouping_out)
}

fn set_or_append(pairs: &mut Vec<(String, J)>, key: &str, val: J) {
    if let Some((_, v)) = pairs.iter_mut().find(|(k, _)| k == key) {
        *v = val;
    } else {
        pairs.push((key.to_string(), val));
    }
}

fn make_package_resource_conformance(
    c: &ConformanceRes,
    cr: Option<&ConfigResource>,
    add_group: &mut impl FnMut(&str, Option<&str>, Option<&str>),
) -> ResEntry {
    let mut pairs: Vec<(String, J)> = Vec::new();
    let mut refmap = Map::new();
    refmap.insert("reference".into(), J::String(c.reference_key.clone()));
    pairs.push(("reference".into(), J::Object(refmap)));

    let name = cr
        .and_then(|c| c.name.clone())
        .or_else(|| c.name.clone());
    let description = cr
        .and_then(|c| c.description.clone())
        .or_else(|| c.description.clone());
    if let Some(n) = &name {
        pairs.push(("name".into(), J::String(n.clone())));
    }
    if let Some(d) = &description {
        pairs.push(("description".into(), J::String(d.clone())));
    }
    if let Some(fv) = cr.and_then(|c| c.fhir_version.as_ref()) {
        if !fv.is_empty() {
            pairs.push((
                "fhirVersion".into(),
                J::Array(fv.iter().cloned().map(J::String).collect()),
            ));
        }
    }
    if let Some(gid) = cr.and_then(|c| c.grouping_id.clone()) {
        pairs.push(("groupingId".into(), J::String(gid.clone())));
        add_group(&gid, None, None);
    }
    // example flag
    if let Some(ec) = cr.and_then(|c| c.example_canonical.clone()) {
        pairs.push(("exampleCanonical".into(), J::String(ec)));
    } else if let Some(eb) = cr.and_then(|c| c.example_boolean) {
        pairs.push(("exampleBoolean".into(), J::Bool(eb)));
    } else {
        pairs.push(("exampleBoolean".into(), J::Bool(false)));
    }
    if let Some(ext) = cr.and_then(|c| c.extension.clone()) {
        if ext.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            pairs.push(("extension".into(), ext));
        }
    }

    let sort_name = name.unwrap_or_else(|| c.reference_key.clone());
    ResEntry {
        reference_key: c.reference_key.clone(),
        pairs,
        sort_name,
    }
}

fn make_package_resource_instance(
    inst: &IgInstanceMeta,
    cr: Option<&ConfigResource>,
    inputs: &IgInputs,
    cfg: &Config,
    add_group: &mut impl FnMut(&str, Option<&str>, Option<&str>),
) -> ResEntry {
    let mut pairs: Vec<(String, J)> = Vec::new();
    let mut refmap = Map::new();
    refmap.insert("reference".into(), J::String(inst.reference_key.clone()));
    pairs.push(("reference".into(), J::Object(refmap)));

    let name = cr.and_then(|c| c.name.clone()).or_else(|| inst.name.clone());
    let description = cr
        .and_then(|c| c.description.clone())
        .or_else(|| inst.description.clone());
    if let Some(n) = &name {
        pairs.push(("name".into(), J::String(n.clone())));
    }
    if let Some(d) = &description {
        pairs.push(("description".into(), J::String(d.clone())));
    }
    if let Some(fv) = cr.and_then(|c| c.fhir_version.as_ref()) {
        if !fv.is_empty() {
            pairs.push((
                "fhirVersion".into(),
                J::Array(fv.iter().cloned().map(J::String).collect()),
            ));
        }
    }
    if let Some(gid) = cr.and_then(|c| c.grouping_id.clone()) {
        pairs.push(("groupingId".into(), J::String(gid.clone())));
        add_group(&gid, None, None);
    }
    // example flag
    if let Some(ec) = cr.and_then(|c| c.example_canonical.clone()) {
        pairs.push(("exampleCanonical".into(), J::String(ec)));
    } else if let Some(eb) = cr.and_then(|c| c.example_boolean) {
        pairs.push(("exampleBoolean".into(), J::Bool(eb)));
    } else if inst.usage == "Example" {
        // find first of [meta.profile..., instanceOfUrl] that fishes a local Profile/Logical
        let mut candidates: Vec<String> = inst.meta_profile.clone();
        if let Some(iou) = &inst.instance_of_url {
            candidates.push(iou.clone());
        }
        let example_url = candidates.into_iter().find(|url| {
            let mut parts = url.splitn(2, '|');
            let base = parts.next().unwrap_or("");
            let ver = parts.next();
            match inputs.local_profile_logical.get(base) {
                Some(local_ver) => match ver {
                    None => true,
                    Some(v) => {
                        // version === (availableProfileOrLogical.version ?? config.version)
                        let effective =
                            if local_ver.is_empty() { cfg.version.clone().unwrap_or_default() } else { local_ver.clone() };
                        v == effective
                    }
                },
                None => false,
            }
        });
        if let Some(eu) = example_url {
            let base = eu.split('|').next().unwrap_or(&eu).to_string();
            pairs.push(("exampleCanonical".into(), J::String(base)));
        } else {
            pairs.push(("exampleBoolean".into(), J::Bool(true)));
        }
    } else {
        pairs.push(("exampleBoolean".into(), J::Bool(false)));
    }
    let mut had_format_ext = false;
    if let Some(ext) = cr.and_then(|c| c.extension.clone()) {
        if ext.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            if let Some(arr) = ext.as_array() {
                had_format_ext = arr.iter().any(|e| {
                    e.get("url").and_then(|u| u.as_str()).map(is_resource_format_ext).unwrap_or(false)
                });
            }
            pairs.push(("extension".into(), ext));
        }
    }
    // logical-model instances get a resource-format extension.
    if inst.logical && !had_format_ext {
        let new_ext = {
            let mut m = Map::new();
            m.insert(
                "url".into(),
                J::String("http://hl7.org/fhir/tools/StructureDefinition/implementationguide-resource-format".into()),
            );
            m.insert("valueCode".into(), J::String("application/fhir+json".into()));
            J::Object(m)
        };
        if let Some((_, v)) = pairs.iter_mut().find(|(k, _)| k == "extension") {
            if let Some(arr) = v.as_array_mut() {
                arr.push(new_ext);
            }
        } else {
            pairs.push(("extension".into(), J::Array(vec![new_ext])));
        }
    }

    let sort_name = name.unwrap_or_else(|| inst.reference_key.clone());
    ResEntry {
        reference_key: inst.reference_key.clone(),
        pairs,
        sort_name,
    }
}

fn is_resource_format_ext(url: &str) -> bool {
    url == "http://hl7.org/fhir/tools/StructureDefinition/implementationguide-resource-format"
        || url == "http://hl7.org/fhir/StructureDefinition/implementationguide-resource-format"
}

fn make_config_only_resource(c: &ConfigResource) -> ResEntry {
    let mut pairs: Vec<(String, J)> = Vec::new();
    let mut refmap = Map::new();
    refmap.insert("reference".into(), J::String(c.reference.clone()));
    pairs.push(("reference".into(), J::Object(refmap)));
    // spread details (authoring order), excluding profile/fhirVersion (re-added)
    for (k, v) in &c.ordered {
        if k == "profile" || k == "fhirVersion" {
            continue;
        }
        pairs.push((k.clone(), v.clone()));
    }
    if let Some(p) = &c.profile {
        pairs.push(("profile".into(), J::Array(p.iter().cloned().map(J::String).collect())));
    }
    if let Some(fv) = &c.fhir_version {
        pairs.push((
            "fhirVersion".into(),
            J::Array(fv.iter().cloned().map(J::String).collect()),
        ));
    }
    let sort_name = c.name.clone().unwrap_or_else(|| c.reference.clone());
    ResEntry {
        reference_key: c.reference.clone(),
        pairs,
        sort_name,
    }
}

// ---- predefined resources -------------------------------------------------

struct PredefinedRes {
    resource_type: String,
    id: String,
    title: Option<String>,
    name: Option<String>,
    url: Option<String>,
    description: Option<String>,
    /// basename of the containing folder (e.g. "resources", "examples").
    folder: String,
    /// file stem (filename without extension).
    file_stem: String,
    meta_profile: Vec<String>,
}

fn add_predefined_resources(
    entries: &mut Vec<ResEntry>,
    cfg_yaml: &Y,
    inputs: &IgInputs,
    config_resources: &[ConfigResource],
    cfg: &Config,
    is_r4: bool,
    add_group: &mut impl FnMut(&str, Option<&str>, Option<&str>),
) {
    let _ = is_r4;
    let files = collect_predefined_files(&inputs.ig_dir, cfg_yaml);
    // configured Binary resources with a resource-format extension.
    let configured_binary: Vec<&ConfigResource> = config_resources
        .iter()
        .filter(|c| {
            c.reference.starts_with("Binary/")
                && c.extension
                    .as_ref()
                    .and_then(|e| e.as_array())
                    .map(|a| {
                        a.iter().any(|e| {
                            e.get("url")
                                .and_then(|u| u.as_str())
                                .map(is_resource_format_ext)
                                .unwrap_or(false)
                        })
                    })
                    .unwrap_or(false)
        })
        .collect();

    for pf in files {
        // Binary-skip: a configured Binary reference covers this file.
        let binary_match = configured_binary.iter().any(|c| {
            (c.reference == format!("Binary/{}", pf.id)
                && (c.example_canonical.as_deref()
                    == Some(&format!("{}/StructureDefinition/{}", cfg.canonical, pf.resource_type))
                    || c.example_canonical.as_deref() == Some(pf.resource_type.as_str())))
                || c.reference == format!("Binary/{}", pf.file_stem)
        });
        if binary_match {
            continue;
        }
        if pf.resource_type.is_empty() || pf.id.is_empty() {
            continue;
        }
        let reference_key = format!("{}/{}", pf.resource_type, pf.id);
        let cr = find_config_resource(config_resources, &reference_key);
        if cr.map(|c| c.omit).unwrap_or(false) {
            continue;
        }
        // existing FSH/instance entry (replace in place).
        let existing_index = entries.iter().position(|e| e.reference_key == reference_key);
        let existing = existing_index.map(|i| &entries[i]);
        let existing_is_example = existing
            .map(|e| {
                e.pairs.iter().any(|(k, v)| {
                    (k == "exampleBoolean" && v.as_bool() == Some(true)) || k == "exampleCanonical"
                })
            })
            .unwrap_or(false);
        let existing_name = if existing_is_example {
            existing.and_then(|e| pair_str(&e.pairs, "name"))
        } else {
            None
        };
        let existing_description = if existing_is_example {
            existing.and_then(|e| pair_str(&e.pairs, "description"))
        } else {
            None
        };

        let entry = make_predefined_resource(
            &pf,
            cr,
            &reference_key,
            existing_name,
            existing_description,
            inputs,
            cfg,
            add_group,
        );
        if let Some(i) = existing_index {
            entries[i] = entry;
        } else {
            entries.push(entry);
        }
    }
}

fn pair_str(pairs: &[(String, J)], key: &str) -> Option<String> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_str().map(str::to_string))
}

#[allow(clippy::too_many_arguments)]
fn make_predefined_resource(
    pf: &PredefinedRes,
    cr: Option<&ConfigResource>,
    reference_key: &str,
    existing_name: Option<String>,
    existing_description: Option<String>,
    inputs: &IgInputs,
    cfg: &Config,
    add_group: &mut impl FnMut(&str, Option<&str>, Option<&str>),
) -> ResEntry {
    let mut pairs: Vec<(String, J)> = Vec::new();
    let mut refmap = Map::new();
    refmap.insert("reference".into(), J::String(reference_key.to_string()));
    pairs.push(("reference".into(), J::Object(refmap)));

    let is_conformance = is_conformance_type(&pf.resource_type);
    let meta_ext_name = if is_conformance { None } else { pf.name.clone() }; // approx: no meta.extension parsing
    let _ = meta_ext_name;

    // description (set before example/name)
    let description = cr
        .and_then(|c| c.description.clone())
        .or(existing_description)
        .or_else(|| pf.description.clone());
    if let Some(d) = &description {
        pairs.push(("description".into(), J::String(d.clone())));
    }
    if let Some(fv) = cr.and_then(|c| c.fhir_version.as_ref()) {
        if !fv.is_empty() {
            pairs.push((
                "fhirVersion".into(),
                J::Array(fv.iter().cloned().map(J::String).collect()),
            ));
        }
    }
    if let Some(gid) = cr.and_then(|c| c.grouping_id.clone()) {
        pairs.push(("groupingId".into(), J::String(gid.clone())));
        add_group(&gid, None, None);
    }

    let is_examples_folder = pf.folder == "examples";
    let name = cr
        .and_then(|c| c.name.clone())
        .or(existing_name)
        .or_else(|| pf.title.clone())
        .or_else(|| pf.name.clone())
        .unwrap_or_else(|| pf.id.clone());

    if is_examples_folder {
        // examples: name BEFORE example flag.
        pairs.push(("name".into(), J::String(name.clone())));
        push_predefined_example_flag(&mut pairs, cr, pf, true, inputs, cfg);
    } else {
        // non-examples: example flag BEFORE name.
        push_predefined_example_flag(&mut pairs, cr, pf, false, inputs, cfg);
        pairs.push(("name".into(), J::String(name.clone())));
    }

    if let Some(ext) = cr.and_then(|c| c.extension.clone()) {
        if ext.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            pairs.push(("extension".into(), ext));
        }
    }

    ResEntry {
        reference_key: reference_key.to_string(),
        pairs,
        sort_name: name,
    }
}

fn push_predefined_example_flag(
    pairs: &mut Vec<(String, J)>,
    cr: Option<&ConfigResource>,
    pf: &PredefinedRes,
    examples_folder: bool,
    inputs: &IgInputs,
    cfg: &Config,
) {
    if let Some(ec) = cr.and_then(|c| c.example_canonical.clone()) {
        pairs.push(("exampleCanonical".into(), J::String(ec)));
    } else if let Some(eb) = cr.and_then(|c| c.example_boolean) {
        pairs.push(("exampleBoolean".into(), J::Bool(eb)));
    } else if examples_folder {
        // fish meta.profile for a local profile.
        let example_url = pf.meta_profile.iter().find(|url| {
            let base = url.split('|').next().unwrap_or(url);
            inputs.local_profile_logical.contains_key(base)
        });
        if let Some(eu) = example_url {
            let base = eu.split('|').next().unwrap_or(eu).to_string();
            pairs.push(("exampleCanonical".into(), J::String(base)));
        } else {
            pairs.push(("exampleBoolean".into(), J::Bool(true)));
        }
    } else {
        pairs.push(("exampleBoolean".into(), J::Bool(false)));
    }
    let _ = cfg;
}

/// CONFORMANCE_AND_TERMINOLOGY_RESOURCES (`fhirtypes/common.ts`).
fn is_conformance_type(rt: &str) -> bool {
    matches!(
        rt,
        "CapabilityStatement"
            | "CapabilityStatement2"
            | "StructureDefinition"
            | "ImplementationGuide"
            | "SearchParameter"
            | "MessageDefinition"
            | "OperationDefinition"
            | "CompartmentDefinition"
            | "StructureMap"
            | "GraphDefinition"
            | "ExampleScenario"
            | "CodeSystem"
            | "ValueSet"
            | "ConceptMap"
            | "ConceptMap2"
            | "NamingSystem"
            | "TerminologyCapabilities"
    )
}

/// Build a name/id/url -> canonical-url lookup of predefined ValueSet resources
/// (loaded from `input/resources` etc.). Used by the SD binding fisher so that a
/// `* path from <Name>` binding resolves to a locally-defined ValueSet's url
/// before falling through to the FHIR packages (which may carry a wrong same-named
/// THO/core ValueSet, or none at all).
pub fn predefined_vs_map(
    ig_dir: &str,
    cfg_yaml: &Y,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for pf in collect_predefined_files(ig_dir, cfg_yaml) {
        if pf.resource_type != "ValueSet" {
            continue;
        }
        let Some(url) = pf.url.clone() else { continue };
        // First writer wins (folder iteration order); keys: name, id, url.
        if let Some(n) = &pf.name {
            map.entry(n.clone()).or_insert_with(|| url.clone());
        }
        if !pf.id.is_empty() {
            map.entry(pf.id.clone()).or_insert_with(|| url.clone());
        }
        map.entry(url.clone()).or_insert(url);
    }
    map
}

/// Enumerate predefined resource files in the recognized input folders.
fn collect_predefined_files(ig_dir: &str, cfg_yaml: &Y) -> Vec<PredefinedRes> {
    let input = Path::new(ig_dir).join("input");
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    for end in [
        "capabilities",
        "extensions",
        "models",
        "operations",
        "profiles",
        "resources",
        "vocabulary",
        "examples",
    ] {
        let p = input.join(end);
        if p.is_dir() {
            dirs.push(p);
        }
    }
    // path-resource parameters (relative to project dir).
    if let Some(Y::Mapping(pm)) = yget(cfg_yaml, "parameters") {
        for (k, v) in pm {
            if ystr(k).as_deref() == Some("path-resource") {
                for val in norm_array(v) {
                    if let Some(s) = ystr(&val) {
                        let rel = s.trim_end_matches("/*");
                        let full = Path::new(ig_dir).join(rel.replace('/', std::path::MAIN_SEPARATOR_STR));
                        if full.is_dir() {
                            dirs.push(full);
                        }
                    }
                }
            }
        }
    }

    let mut out = Vec::new();
    for dir in dirs {
        let folder = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<std::path::PathBuf> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        for f in files {
            let ext = f
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            if ext != "json" && ext != "xml" {
                continue;
            }
            let stem = f
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            if let Some(pf) = parse_predefined_file(&f, &ext, &folder, &stem) {
                out.push(pf);
            }
        }
    }
    out
}

fn parse_predefined_file(
    path: &Path,
    ext: &str,
    folder: &str,
    stem: &str,
) -> Option<PredefinedRes> {
    let bytes = std::fs::read(path).ok()?;
    if ext == "json" {
        let json: J = serde_json::from_slice(&bytes).ok()?;
        let rt = json.get("resourceType").and_then(|v| v.as_str())?;
        let id = json
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let meta_profile = json
            .get("meta")
            .and_then(|m| m.get("profile"))
            .and_then(|p| p.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        Some(PredefinedRes {
            resource_type: rt.to_string(),
            id,
            title: json.get("title").and_then(|v| v.as_str()).map(str::to_string),
            name: json.get("name").and_then(|v| v.as_str()).map(str::to_string),
            url: json.get("url").and_then(|v| v.as_str()).map(str::to_string),
            description: json
                .get("description")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            folder: folder.to_string(),
            file_stem: stem.to_string(),
            meta_profile,
        })
    } else {
        let text = String::from_utf8_lossy(&bytes);
        parse_predefined_xml(&text, folder, stem)
    }
}

/// Lightweight FHIR-XML field extraction: root element = resourceType; first-level
/// children `id`/`name`/`title`/`description` `value` attributes; meta.profile.
fn parse_predefined_xml(text: &str, folder: &str, stem: &str) -> Option<PredefinedRes> {
    let toks = xml_tokenize(text);
    // find root element (first start tag that isn't the xml declaration)
    let mut depth = 0i32;
    let mut root: Option<String> = None;
    let mut id = String::new();
    let mut title = None;
    let mut name = None;
    let mut url = None;
    let mut description = None;
    let mut meta_profile = Vec::new();
    let mut in_meta_depth: Option<i32> = None;
    for t in &toks {
        match t {
            XmlTok::Start { tag, attrs, self_closing } => {
                depth += 1;
                if root.is_none() {
                    root = Some(tag.clone());
                    if *self_closing {
                        depth -= 1;
                    }
                    continue;
                }
                let rel = depth - 1; // depth relative to root (root children at rel==1)
                if rel == 1 {
                    let val = attr_value(attrs);
                    match tag.as_str() {
                        "id" if id.is_empty() => id = val.unwrap_or_default(),
                        "name" if name.is_none() => name = val,
                        "url" if url.is_none() => url = val,
                        "title" if title.is_none() => title = val,
                        "description" if description.is_none() => description = val,
                        "meta" => in_meta_depth = Some(depth),
                        _ => {}
                    }
                }
                if let Some(md) = in_meta_depth {
                    if depth == md + 1 && tag == "profile" {
                        if let Some(v) = attr_value(attrs) {
                            meta_profile.push(v);
                        }
                    }
                }
                if *self_closing {
                    depth -= 1;
                    if let Some(md) = in_meta_depth {
                        if depth < md {
                            in_meta_depth = None;
                        }
                    }
                }
            }
            XmlTok::End { .. } => {
                depth -= 1;
                if let Some(md) = in_meta_depth {
                    if depth < md {
                        in_meta_depth = None;
                    }
                }
            }
            XmlTok::Other => {}
        }
    }
    let rt = root?;
    Some(PredefinedRes {
        resource_type: rt,
        id,
        title,
        name,
        url,
        description,
        folder: folder.to_string(),
        file_stem: stem.to_string(),
        meta_profile,
    })
}

fn attr_value(attrs: &[(String, String)]) -> Option<String> {
    attrs
        .iter()
        .find(|(k, _)| k == "value")
        .map(|(_, v)| v.clone())
}

enum XmlTok {
    Start {
        tag: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    End {
        #[allow(dead_code)]
        tag: String,
    },
    Other,
}

/// Minimal XML tokenizer (tags + attributes). Skips comments, declarations,
/// processing instructions, CDATA; ignores text nodes.
fn xml_tokenize(text: &str) -> Vec<XmlTok> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    let n = bytes.len();
    while i < n {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // comment / declaration / PI / CDATA
        if text[i..].starts_with("<!--") {
            if let Some(end) = text[i..].find("-->") {
                i += end + 3;
            } else {
                break;
            }
            out.push(XmlTok::Other);
            continue;
        }
        if text[i..].starts_with("<![CDATA[") {
            if let Some(end) = text[i..].find("]]>") {
                i += end + 3;
            } else {
                break;
            }
            out.push(XmlTok::Other);
            continue;
        }
        if text[i..].starts_with("<?") || text[i..].starts_with("<!") {
            if let Some(end) = text[i..].find('>') {
                i += end + 1;
            } else {
                break;
            }
            out.push(XmlTok::Other);
            continue;
        }
        // find end of tag
        let Some(rel_end) = text[i..].find('>') else {
            break;
        };
        let tag_text = &text[i + 1..i + rel_end];
        i += rel_end + 1;
        if let Some(rest) = tag_text.strip_prefix('/') {
            out.push(XmlTok::End {
                tag: rest.trim().to_string(),
            });
            continue;
        }
        let self_closing = tag_text.ends_with('/');
        let inner = tag_text.trim_end_matches('/').trim();
        // tag name
        let mut parts = inner.splitn(2, |c: char| c.is_whitespace());
        let tag = parts.next().unwrap_or("").to_string();
        let attrs = parse_xml_attrs(parts.next().unwrap_or(""));
        out.push(XmlTok::Start {
            tag,
            attrs,
            self_closing,
        });
    }
    out
}

fn parse_xml_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    while i < n {
        // skip whitespace
        while i < n && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        let start = i;
        while i < n && bytes[i] != b'=' && !(bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if start == i {
            break;
        }
        let key = &s[start..i];
        while i < n && (bytes[i] as char).is_whitespace() {
            i += 1;
        }
        if i < n && bytes[i] == b'=' {
            i += 1;
            while i < n && (bytes[i] as char).is_whitespace() {
                i += 1;
            }
            if i < n && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let vstart = i;
                while i < n && bytes[i] != quote {
                    i += 1;
                }
                let val = xml_unescape(&s[vstart..i]);
                out.push((key.to_string(), val));
                i += 1;
            }
        }
    }
    out
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn sort_resources(entries: &mut [ResEntry], config_resources: &[ConfigResource], groups: &[Group]) {
    // trySortResourcesByConfig
    if !config_resources.is_empty() {
        let all_in: Option<Vec<usize>> = entries
            .iter()
            .map(|e| {
                config_resources
                    .iter()
                    .position(|c| c.reference == e.reference_key)
            })
            .collect();
        if let Some(indices) = all_in {
            let mut order: Vec<usize> = (0..entries.len()).collect();
            order.sort_by_key(|&i| indices[i]);
            apply_order(entries, order);
            return;
        }
    }
    // trySortResourcesByGroup
    if !groups.is_empty() {
        let all_in: Option<Vec<(usize, usize)>> = entries
            .iter()
            .map(|e| {
                for (gi, g) in groups.iter().enumerate() {
                    if let Some(ri) = g.resources.iter().position(|r| r == &e.reference_key) {
                        return Some((gi, ri));
                    }
                }
                None
            })
            .collect();
        if let Some(keys) = all_in {
            let mut order: Vec<usize> = (0..entries.len()).collect();
            order.sort_by_key(|&i| keys[i]);
            apply_order(entries, order);
            return;
        }
    }
    // fallback: by uppercased sort_name (stable)
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by(|&a, &b| {
        entries[a]
            .sort_name
            .to_uppercase()
            .cmp(&entries[b].sort_name.to_uppercase())
    });
    apply_order(entries, order);
}

fn apply_order(entries: &mut [ResEntry], order: Vec<usize>) {
    // reorder entries in place using the permutation `order`
    let mut taken: Vec<Option<ResEntry>> = entries
        .iter_mut()
        .map(|e| {
            Some(ResEntry {
                reference_key: std::mem::take(&mut e.reference_key),
                pairs: std::mem::take(&mut e.pairs),
                sort_name: std::mem::take(&mut e.sort_name),
            })
        })
        .collect();
    for (dst, &src) in order.iter().enumerate() {
        entries[dst] = taken[src].take().unwrap();
    }
}

fn transform_resource_entry(pairs: &mut Vec<(String, J)>, is_r4: bool) {
    if is_r4 {
        // translateR5PropertiesToR4: isExample → exampleBoolean handled at config
        // level; nothing to do for the common path (exampleBoolean/Canonical stay).
        return;
    }
    // updateForR5: exampleBoolean/exampleCanonical → isExample (+profile).
    let example_canonical = pairs
        .iter()
        .find(|(k, _)| k == "exampleCanonical")
        .and_then(|(_, v)| v.as_str().map(str::to_string));
    let example_boolean = pairs
        .iter()
        .find(|(k, _)| k == "exampleBoolean")
        .and_then(|(_, v)| v.as_bool());
    let truthy = example_canonical.is_some() || example_boolean == Some(true);
    if truthy {
        let canon = example_canonical.clone();
        pairs.retain(|(k, _)| k != "exampleBoolean" && k != "exampleCanonical");
        pairs.push(("isExample".into(), J::Bool(true)));
        if let Some(c) = canon {
            pairs.push(("profile".into(), J::Array(vec![J::String(c)])));
        }
    } else if example_boolean == Some(false) {
        pairs.retain(|(k, _)| k != "exampleBoolean");
        pairs.push(("isExample".into(), J::Bool(false)));
    }
}

// ---- page -----------------------------------------------------------------

fn build_page(cfg_yaml: &Y, is_r4: bool, ig_dir: &str) -> J {
    // root toc page
    let mut children: Vec<J> = Vec::new();
    let has_config_pages = matches!(yget(cfg_yaml, "pages"), Some(Y::Mapping(m)) if !m.is_empty());
    if has_config_pages {
        if let Some(Y::Mapping(pages)) = yget(cfg_yaml, "pages") {
            for (k, v) in pages {
                if let Some(p) = build_configured_page(k, v, is_r4, cfg_yaml) {
                    children.push(p);
                }
            }
        }
    } else {
        // addIndex + addOtherPageContent (disk scan).
        children.extend(build_disk_pages(ig_dir, is_r4));
    }

    if is_r4 {
        let mut o = Map::new();
        o.insert("nameUrl".into(), J::String("toc.html".into()));
        o.insert("title".into(), J::String("Table of Contents".into()));
        o.insert("generation".into(), J::String("html".into()));
        o.insert("page".into(), J::Array(children));
        J::Object(o)
    } else {
        // R5: title, generation, page, name, sourceUrl
        let mut o = Map::new();
        o.insert("title".into(), J::String("Table of Contents".into()));
        o.insert("generation".into(), J::String("html".into()));
        o.insert("page".into(), J::Array(children));
        o.insert("name".into(), J::String("toc.html".into()));
        o.insert("sourceUrl".into(), J::String("toc.html".into()));
        J::Object(o)
    }
}

/// Build one configured page (recursive). `name_url` is the YAML key.
fn build_configured_page(name_key: &Y, details: &Y, is_r4: bool, _cfg_yaml: &Y) -> Option<J> {
    let name_url = ystr(name_key)?;
    let (name, file_type) = match name_url.rfind('.') {
        None => (name_url.clone(), String::new()),
        Some(p) => (name_url[..p].to_string(), name_url[p + 1..].to_string()),
    };
    let title = yget_str(details, "title")
        .unwrap_or_else(|| title_case_from_name(&name));
    let generation = yget_str(details, "generation").unwrap_or_else(|| {
        if file_type == "md" {
            "markdown".into()
        } else {
            "html".into()
        }
    });
    let extension = yget(details, "extension").map(yaml_to_json);
    let config_name = yget_str(details, "name");
    let source_url = yget_str(details, "sourceUrl");

    // recurse into sub-pages
    let mut subpages: Vec<J> = Vec::new();
    if let Y::Mapping(dm) = details {
        for (k, v) in dm {
            let Some(ks) = ystr(k) else { continue };
            if matches!(
                ks.as_str(),
                "title"
                    | "generation"
                    | "sourceUrl"
                    | "sourceString"
                    | "sourceMarkdown"
                    | "extension"
                    | "modifierExtension"
                    | "name"
            ) {
                continue;
            }
            if v.is_mapping() {
                if let Some(sp) = build_configured_page(k, v, is_r4, _cfg_yaml) {
                    subpages.push(sp);
                }
            }
        }
    }

    let mut o = Map::new();
    if is_r4 {
        o.insert("nameUrl".into(), J::String(format!("{name}.html")));
        o.insert("title".into(), J::String(title));
        o.insert("generation".into(), J::String(generation));
        if let Some(e) = &extension {
            o.insert("extension".into(), e.clone());
        }
        if !subpages.is_empty() {
            o.insert("page".into(), J::Array(subpages));
        }
        // R4: add name/source extensions if config name/sourceUrl differ
        add_r4_page_extensions(&mut o, &name_url, config_name.as_deref(), source_url.as_deref());
    } else {
        // R5: title, generation, [extension], [page], name, sourceUrl
        o.insert("title".into(), J::String(title));
        o.insert("generation".into(), J::String(generation));
        if let Some(e) = &extension {
            o.insert("extension".into(), e.clone());
        }
        if !subpages.is_empty() {
            o.insert("page".into(), J::Array(subpages));
        }
        let r5_name = config_name.clone().unwrap_or_else(|| format!("{name}.html"));
        o.insert("name".into(), J::String(r5_name.clone()));
        // sourceUrl: config sourceUrl, else default to name
        let src = source_url.clone().unwrap_or_else(|| format!("{name}.html"));
        o.insert("sourceUrl".into(), J::String(src));
    }
    Some(J::Object(o))
}

fn add_r4_page_extensions(
    o: &mut Map<String, J>,
    name_url: &str,
    config_name: Option<&str>,
    source_url: Option<&str>,
) {
    let mut exts: Vec<J> = o
        .get("extension")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let before = exts.len();
    if let Some(cn) = config_name {
        if cn != name_url {
            let mut m = Map::new();
            m.insert("url".into(), J::String("http://hl7.org/fhir/5.0/StructureDefinition/extension-ImplementationGuide.definition.page.name".into()));
            m.insert("valueUrl".into(), J::String(cn.to_string()));
            exts.push(J::Object(m));
        }
    }
    if let Some(su) = source_url {
        if su != name_url {
            let mut m = Map::new();
            m.insert("url".into(), J::String("http://hl7.org/fhir/5.0/StructureDefinition/extension-ImplementationGuide.definition.page.source".into()));
            m.insert("valueUrl".into(), J::String(su.to_string()));
            exts.push(J::Object(m));
        }
    }
    if exts.len() != before || o.contains_key("extension") {
        if !exts.is_empty() {
            o.insert("extension".into(), J::Array(exts));
        }
    }
}

/// `titleCase(words(name).join(' '))`.
fn title_case_from_name(name: &str) -> String {
    title_case(&words(name).join(" "))
}

// ---- disk page scan (addIndex + addOtherPageContent) ----------------------

struct DiskPage {
    original_name: String,
    prefix: Option<i64>,
    name: String,
    title: String,
    file_type: String,
}

fn build_disk_pages(ig_dir: &str, is_r4: bool) -> Vec<J> {
    let mut out: Vec<J> = Vec::new();

    // addIndex: only if an index.md/.xml exists in pagecontent or pages.
    let input = Path::new(ig_dir).join("input");
    let idx_md_pc = input.join("pagecontent").join("index.md");
    let idx_xml_pc = input.join("pagecontent").join("index.xml");
    let idx_md_pg = input.join("pages").join("index.md");
    let idx_xml_pg = input.join("pages").join("index.xml");
    let has_index = idx_md_pc.exists() || idx_xml_pc.exists() || idx_md_pg.exists() || idx_xml_pg.exists();
    if has_index {
        // generation: markdown unless only an xml index exists.
        let generation = if !idx_md_pg.exists()
            && !idx_md_pc.exists()
            && (idx_xml_pg.exists() || idx_xml_pc.exists())
        {
            "html"
        } else {
            "markdown"
        };
        out.push(make_disk_page("index.html", "Home", generation, is_r4));
    }

    // addOtherPageContent: pagecontent, pages, resource-docs.
    for folder in ["pagecontent", "pages", "resource-docs"] {
        let dir = input.join(folder);
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = rd
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort(); // stable base; compareIgFilenames is the real order
        let organized = organize_page_content(&names);
        for p in organized {
            let supported = p.file_type == "md" || p.file_type == "xml";
            let intro_notes = p.name.ends_with("-intro") || p.name.ends_with("-notes");
            if supported && !intro_notes {
                let generation = if p.file_type == "md" { "markdown" } else { "html" };
                out.push(make_disk_page(
                    &format!("{}.html", p.name),
                    &p.title,
                    generation,
                    is_r4,
                ));
            }
        }
    }
    out
}

fn make_disk_page(name_url: &str, title: &str, generation: &str, is_r4: bool) -> J {
    let mut o = Map::new();
    if is_r4 {
        o.insert("nameUrl".into(), J::String(name_url.to_string()));
        o.insert("title".into(), J::String(title.to_string()));
        o.insert("generation".into(), J::String(generation.to_string()));
    } else {
        o.insert("title".into(), J::String(title.to_string()));
        o.insert("generation".into(), J::String(generation.to_string()));
        o.insert("name".into(), J::String(name_url.to_string()));
        o.insert("sourceUrl".into(), J::String(name_url.to_string()));
    }
    J::Object(o)
}

fn organize_page_content(pages: &[String]) -> Vec<DiskPage> {
    // remove duplicate base names (keep all but log skipped — we just keep first).
    let mut filtered: Vec<&String> = Vec::new();
    for p in pages {
        let base = &p[..p.rfind('.').unwrap_or(p.len())];
        let first = pages
            .iter()
            .find(|q| &q[..q.rfind('.').unwrap_or(q.len())] == base);
        if first == Some(p) {
            filtered.push(p);
        }
    }

    let mut data: Vec<DiskPage> = filtered
        .iter()
        .map(|page| {
            let last_dot = page.rfind('.').unwrap_or(page.len());
            let name_with_prefix = page[..last_dot].to_string();
            // ^(\d+)_(.*)
            let (prefix, name_without_prefix) = parse_numeric_prefix(page);
            let file_type = if last_dot < page.len() {
                page[last_dot + 1..].to_string()
            } else {
                String::new()
            };
            DiskPage {
                original_name: (*page).clone(),
                prefix,
                name: name_with_prefix,
                title: title_case(&words(&name_without_prefix).join(" ")),
                file_type,
            }
        })
        .collect();

    // de-dup name collisions: if names collide, use originalName-minus-ext.
    loop {
        let mut changed = false;
        let names: Vec<String> = data.iter().map(|d| d.name.clone()).collect();
        for d in &mut data {
            if names.iter().filter(|n| **n == d.name).count() > 1 {
                let nn = d.original_name[..d.original_name.rfind('.').unwrap_or(d.original_name.len())]
                    .to_string();
                if nn != d.name {
                    d.name = nn;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }

    data.retain(|d| d.name != "index");
    data.sort_by(compare_ig_filenames);
    data
}

/// `^(\d+)_(.*)` → (prefix, nameWithoutPrefix-minus-ext). Else (None, name-minus-ext).
fn parse_numeric_prefix(page: &str) -> (Option<i64>, String) {
    let last_dot = page.rfind('.').unwrap_or(page.len());
    if let Some(us) = page.find('_') {
        let digits = &page[..us];
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let rest = &page[us + 1..];
            let rest_dot = rest.rfind('.').unwrap_or(rest.len());
            return (digits.parse().ok(), rest[..rest_dot].to_string());
        }
    }
    (None, page[..last_dot].to_string())
}

fn compare_ig_filenames(a: &DiskPage, b: &DiskPage) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a.prefix, b.prefix) {
        (None, None) => locale_compare(&a.name, &b.name),
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(pa), Some(pb)) => {
            if pa == pb {
                locale_compare(&a.name, &b.name)
            } else {
                pa.cmp(&pb)
            }
        }
    }
}

/// Approximation of JS `String.localeCompare` (locale default) for ASCII-ish names.
/// Falls back to case-insensitive then case-sensitive ordering.
fn locale_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let la = a.to_lowercase();
    let lb = b.to_lowercase();
    la.cmp(&lb).then_with(|| a.cmp(b))
}

// ---- lodash words + title-case --------------------------------------------

/// lodash `words` (default): split on non-alphanumeric and camelCase boundaries.
fn words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let is_word = |c: char| c.is_alphanumeric();
    let is_upper = |c: char| c.is_uppercase();
    let is_lower = |c: char| c.is_lowercase();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if !is_word(c) {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            i += 1;
            continue;
        }
        // camelCase boundary: lower->Upper, or UPPER+UPPER+lower (acronym end).
        if !cur.is_empty() {
            let prev = chars[i - 1];
            let lower_to_upper = is_lower(prev) && is_upper(c);
            let acronym_end = is_upper(prev)
                && is_upper(c)
                && i + 1 < chars.len()
                && is_lower(chars[i + 1]);
            let digit_boundary = (prev.is_ascii_digit() && c.is_alphabetic())
                || (prev.is_alphabetic() && c.is_ascii_digit());
            if lower_to_upper || acronym_end || digit_boundary {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
        i += 1;
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn small_words() -> &'static std::collections::HashSet<&'static str> {
    use std::sync::OnceLock;
    static SET: OnceLock<std::collections::HashSet<&'static str>> = OnceLock::new();
    SET.get_or_init(|| {
        [
            "a", "ad", "an", "and", "as", "at", "because", "but", "by", "en", "for", "if", "in",
            "neither", "nor", "of", "on", "or", "only", "over", "per", "so", "some", "that",
            "than", "the", "to", "up", "upon", "v", "vs", "versus", "via", "when", "with",
            "without", "yet",
        ]
        .into_iter()
        .collect()
    })
}

/// Port of `title-case@3.0.3`.
fn title_case(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let total = chars.len();
    let mut result = String::new();
    let mut i = 0usize;
    while i < total {
        let c = chars[i];
        // tokenize: [^\s:–—-]+ | .
        let is_sep =
            |ch: char| ch.is_whitespace() || ch == ':' || ch == '\u{2013}' || ch == '\u{2014}' || ch == '-';
        let start = i;
        if is_sep(c) {
            // single-char token
            result.push(c);
            i += 1;
            continue;
        }
        // run of non-sep
        let mut j = i;
        while j < total && !is_sep(chars[j]) {
            j += 1;
        }
        let token: String = chars[start..j].iter().collect();
        let index = start;
        let token_len = j - start;

        let is_manual = is_manual_case(&chars[start..j]);
        let is_small = small_words().contains(token.to_lowercase().as_str());
        let at_edge = index == 0 || index + token_len == total;
        // URL check
        let next_char = chars.get(j).copied();
        let url_ok = next_char != Some(':')
            || chars.get(j + 1).map(|c| c.is_whitespace()).unwrap_or(false);

        if !is_manual && (!is_small || at_edge) && url_ok {
            // uppercase first alphanumeric/latin char
            let mut done = false;
            for ch in token.chars() {
                if !done && is_titlecase_alpha(ch) {
                    result.extend(ch.to_uppercase());
                    done = true;
                } else {
                    result.push(ch);
                }
            }
        } else {
            result.push_str(&token);
        }
        i = j;
    }
    result
}

fn is_titlecase_alpha(c: char) -> bool {
    c.is_ascii_alphanumeric() || ('\u{00C0}'..='\u{00FF}').contains(&c)
}

/// `/.(?=[A-Z]|\..)/` — a char followed by uppercase, or `.` followed by any char.
fn is_manual_case(chars: &[char]) -> bool {
    for k in 0..chars.len() {
        if let Some(&next) = chars.get(k + 1) {
            if next.is_uppercase() {
                return true;
            }
            if next == '.' && chars.get(k + 2).is_some() {
                return true;
            }
        }
    }
    false
}

// ---- parameter ------------------------------------------------------------

fn build_parameters(
    cfg_yaml: &Y,
    _cfg: &Config,
    canonical: &str,
    is_r4: bool,
    has_custom_resources: bool,
) -> Vec<J> {
    // (code, value) pairs in build order.
    let mut params: Vec<(String, String)> = Vec::new();

    if let Some(cy) = yget_str(cfg_yaml, "copyrightYear").or_else(|| yget_str(cfg_yaml, "copyrightyear")) {
        params.push(("copyrightyear".into(), cy));
    }
    if let Some(rl) = yget_str(cfg_yaml, "releaseLabel").or_else(|| yget_str(cfg_yaml, "releaselabel")) {
        params.push(("releaselabel".into(), rl));
    }
    if let Some(Y::Mapping(pm)) = yget(cfg_yaml, "parameters") {
        for (k, v) in pm {
            let Some(code) = ystr(k) else { continue };
            for val in norm_array(v) {
                if let Some(vs) = ystr(&val) {
                    params.push((code.clone(), vs));
                }
            }
        }
    }

    // path-history (HL7 IGs)
    let is_hl7 = canonical.starts_with("http://hl7.org/") || canonical.starts_with("https://hl7.org/");
    if is_hl7 && !params.iter().any(|(c, _)| c == "path-history") {
        params.push((
            "path-history".into(),
            format!("{canonical}/history.html"),
        ));
    }
    // autoload-resources=false if custom resources & not present
    if has_custom_resources && !params.iter().any(|(c, _)| c == "autoload-resources") {
        params.push(("autoload-resources".into(), "false".into()));
    }

    params
        .into_iter()
        .map(|(code, value)| {
            let mut o = Map::new();
            if is_r4 {
                o.insert("code".into(), J::String(code));
            } else {
                // R5: code is a Coding {code, system}
                let mut c = Map::new();
                c.insert("code".into(), J::String(code));
                c.insert(
                    "system".into(),
                    J::String("http://hl7.org/fhir/tools/CodeSystem/ig-parameters".into()),
                );
                o.insert("code".into(), J::Object(c));
            }
            o.insert("value".into(), J::String(value));
            J::Object(o)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_ig_url_scans_package_when_index_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let package_dir = temp.path().join("example.fhir.pkg#1.0.0").join("package");
        std::fs::create_dir_all(&package_dir).unwrap();
        std::fs::write(
            package_dir.join(".index.json"),
            r#"{"index-version":2,"files":[]}"#,
        )
        .unwrap();
        std::fs::write(
            package_dir.join("package.json"),
            r#"{"name":"example.fhir.pkg","version":"1.0.0","canonical":"http://example.org/pkg"}"#,
        )
        .unwrap();
        std::fs::write(
            package_dir.join("ImplementationGuide-example.fhir.pkg.json"),
            r#"{"resourceType":"ImplementationGuide","id":"example.fhir.pkg","packageId":"example.fhir.pkg","version":"1.0.0","url":"http://example.org/pkg/ImplementationGuide/example.fhir.pkg"}"#,
        )
        .unwrap();

        assert_eq!(
            find_dependency_ig_url(temp.path().to_str().unwrap(), "example.fhir.pkg", "1.0.0")
                .as_deref(),
            Some("http://example.org/pkg/ImplementationGuide/example.fhir.pkg")
        );
    }
}
