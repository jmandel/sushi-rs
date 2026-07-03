//! Package loading + resource index (`PackageContext`) with fetch/snapshot
//! memoization, version comparison helpers, and `probe_name`.

#![allow(unused_imports)]
use anyhow::{bail, Context};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::*;

#[derive(Debug)]
pub struct PackageContext {
    by_url: HashMap<String, ResourceIndexEntry>,
    by_id: HashMap<String, PathBuf>,
    by_name: HashMap<String, PathBuf>,
    // Interior-mutability memoization. Equivalent to reading+parsing the resource
    // file on every call: the on-disk packages are immutable for the lifetime of a
    // run, so caching parsed values cannot change output — only avoid repeated
    // disk reads and JSON parses.
    fetch_cache: RefCell<HashMap<String, Option<Rc<Value>>>>,
}

#[derive(Clone, Debug)]
pub(crate) struct ResourceIndexEntry {
    path: PathBuf,
    version: Option<String>,
    local: bool,
    /// The owning npm package id (e.g. `hl7.fhir.uv.extensions.r4`), mirroring
    /// Java's `PackageInformation.getId()`. `None` for local-dir resources (Java
    /// loads those outside the package loader, so `PackageHackerR5` never sees a
    /// package id for them). Derived from the cache path
    /// `.../packages/<id>#<ver>/package/<file>`.
    package_id: Option<String>,
}

impl PackageContext {
    pub fn new(cache_dir: impl AsRef<Path>, packages: &[String]) -> anyhow::Result<Self> {
        let cache_dir = cache_dir.as_ref();
        if !cache_dir.is_dir() {
            bail!(
                "FHIR package cache is not a directory: {}",
                cache_dir.display()
            );
        }
        let mut ctx = Self {
            by_url: HashMap::new(),
            by_id: HashMap::new(),
            by_name: HashMap::new(),
            fetch_cache: RefCell::new(HashMap::new()),
        };
        for package in packages {
            ctx.load_package(cache_dir, package)?;
        }
        Ok(ctx)
    }

    fn load_package(&mut self, cache_dir: &Path, package: &str) -> anyhow::Result<()> {
        // Java's PackageInformation.getId() is the npm package name, i.e. the
        // part of `<id>#<version>` before the `#`.
        let package_id = package.split('#').next().unwrap_or(package).to_string();
        let package_dir = cache_dir.join(package).join("package");
        let index_path = package_dir.join(".index.json");
        let index: Value = serde_json::from_slice(
            &std::fs::read(&index_path)
                .with_context(|| format!("cannot read {}", index_path.display()))?,
        )?;
        let Some(files) = index.get("files").and_then(Value::as_array) else {
            return Ok(());
        };
        let mut loaded = 0usize;
        for entry in files {
            if entry.get("resourceType").and_then(Value::as_str) != Some("StructureDefinition") {
                continue;
            }
            let Some(filename) = entry.get("filename").and_then(Value::as_str) else {
                continue;
            };
            let path = package_dir.join(filename);
            if let Some(id) = entry.get("id").and_then(Value::as_str) {
                self.by_id
                    .entry(id.to_string())
                    .or_insert_with(|| path.clone());
            }
            if let Some(url) = entry.get("url").and_then(Value::as_str) {
                let version = entry
                    .get("version")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                self.insert_url(
                    url,
                    path.clone(),
                    version.clone(),
                    false,
                    Some(package_id.clone()),
                );
                if let Some(version) = entry.get("version").and_then(Value::as_str) {
                    self.by_url.insert(
                        format!("{url}|{version}"),
                        ResourceIndexEntry {
                            path: path.clone(),
                            version: Some(version.to_string()),
                            local: false,
                            package_id: Some(package_id.clone()),
                        },
                    );
                }
            }
            if let Some(name) = probe_name(&path) {
                self.by_name.entry(name).or_insert_with(|| path.clone());
            }
            loaded += 1;
        }
        if loaded == 0 {
            self.scan_package_structure_definitions(&package_dir, &package_id)?;
        }
        Ok(())
    }

    fn scan_package_structure_definitions(
        &mut self,
        package_dir: &Path,
        package_id: &str,
    ) -> anyhow::Result<()> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(package_dir)
            .with_context(|| format!("cannot scan package directory {}", package_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
        files.sort();
        for path in files {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            // Byte-check `resourceType` before the expensive full parse. These
            // scan-fallback packages (e.g. us.nlm.vsac, 15k ValueSet files, 0
            // SDs) would otherwise full-parse every file only to discard it.
            // Only StructureDefinitions get parsed. When the byte-scan can't
            // cleanly read the top-level `resourceType` (Some(other) means
            // definitely-not-SD; None means unknown), fall through to the parse
            // so behaviour is exactly `from_slice(...).resourceType == SD`.
            match scan_top_level_string(&bytes, b"resourceType") {
                Some(rt) if rt != "StructureDefinition" => continue,
                _ => {}
            }
            let Ok(json) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if json.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                // Scan-fallback (empty .index.json) resources have historically
                // been indexed with `local: true` — several packages with empty
                // indexes (e.g. subscriptions-backport.r4) rely on that path
                // taking the full R4→R5 conversion, matching the oracle golden.
                // Preserve that exactly; only record the owning package id (for
                // the PackageHackerR5 removeIf scoping).
                self.index_structure_definition(path, &json, true, Some(package_id.to_string()));
            }
        }
        Ok(())
    }

    pub fn load_local_dir(&mut self, dir: impl AsRef<Path>) -> anyhow::Result<()> {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            bail!(
                "local resource directory is not a directory: {}",
                dir.display()
            );
        }
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir)
            .with_context(|| format!("cannot read local resource directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(path);
            }
        }
        files.sort();
        for path in files {
            let Ok(json) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .ok_or(())
            else {
                continue;
            };
            if json.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                self.index_structure_definition(path, &json, true, None);
            }
        }
        Ok(())
    }

    fn index_structure_definition(
        &mut self,
        path: PathBuf,
        json: &Value,
        local: bool,
        package_id: Option<String>,
    ) {
        if let Some(id) = json.get("id").and_then(Value::as_str) {
            self.by_id.insert(id.to_string(), path.clone());
        }
        if let Some(url) = json.get("url").and_then(Value::as_str) {
            let version = json
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_string);
            self.insert_url(url, path.clone(), version.clone(), local, package_id.clone());
            if let Some(version) = version {
                self.by_url.insert(
                    format!("{url}|{version}"),
                    ResourceIndexEntry {
                        path: path.clone(),
                        version: Some(version),
                        local,
                        package_id: package_id.clone(),
                    },
                );
            }
        }
        if let Some(name) = json.get("name").and_then(Value::as_str) {
            self.by_name.insert(name.to_string(), path);
        }
    }

    fn insert_url(
        &mut self,
        url: &str,
        path: PathBuf,
        version: Option<String>,
        local: bool,
        package_id: Option<String>,
    ) {
        let replace = match self.by_url.get(url) {
            Some(existing) => match (&version, &existing.version) {
                (Some(new), Some(old)) if new != old => later_version(new, old),
                _ => local || !existing.local,
            },
            None => true,
        };
        if replace {
            self.by_url.insert(
                url.to_string(),
                ResourceIndexEntry {
                    path,
                    version,
                    local,
                    package_id,
                },
            );
        }
    }

    pub(crate) fn is_local(&self, query: &str) -> bool {
        self.by_url
            .get(query)
            .map(|entry| entry.local)
            .unwrap_or(false)
    }

    /// The owning npm package id for the resource resolved by `query`, mirroring
    /// Java's `PackageInformation.getId()`. Resolves by url first, then falls back
    /// to matching the resolved path (id/name lookups) to a `by_url` entry.
    /// `None` for local-dir resources or unresolved queries.
    pub(crate) fn package_id_for(&self, query: &str) -> Option<String> {
        if let Some(entry) = self.by_url.get(query) {
            return entry.package_id.clone();
        }
        let path = self.resource_path(query)?;
        self.by_url
            .values()
            .find(|e| &e.path == path)
            .and_then(|e| e.package_id.clone())
    }

    pub fn fetch(&self, query: &str) -> Option<Value> {
        self.fetch_rc(query).map(|rc| (*rc).clone())
    }

    // Memoized parse of the resource file for `query`. Returns the shared parsed
    // value (or `None` if unresolved / unreadable), caching both outcomes so
    // repeated lookups do not re-read or re-parse the immutable package files.
    fn fetch_rc(&self, query: &str) -> Option<Rc<Value>> {
        if let Some(cached) = self.fetch_cache.borrow().get(query) {
            return cached.clone();
        }
        let parsed = self
            .resource_path(query)
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .map(Rc::new);
        self.fetch_cache
            .borrow_mut()
            .insert(query.to_string(), parsed.clone());
        parsed
    }

    fn resource_path(&self, query: &str) -> Option<&PathBuf> {
        self.by_url
            .get(query)
            .map(|e| &e.path)
            .or_else(|| self.by_id.get(query))
            .or_else(|| self.by_name.get(query))
    }
}

pub(crate) fn later_version(new: &str, old: &str) -> bool {
    let new_parts = version_parts(new);
    let old_parts = version_parts(old);
    let max = new_parts.len().max(old_parts.len());
    for i in 0..max {
        let n = new_parts.get(i);
        let o = old_parts.get(i);
        match (n, o) {
            (Some(VersionPart::Number(n)), Some(VersionPart::Number(o))) if n != o => return n > o,
            (Some(VersionPart::Text(n)), Some(VersionPart::Text(o))) if n != o => return n > o,
            (Some(VersionPart::Number(_)), Some(VersionPart::Text(_))) => return true,
            (Some(VersionPart::Text(_)), Some(VersionPart::Number(_))) => return false,
            (Some(VersionPart::Number(n)), None) => return *n > 0,
            (Some(VersionPart::Text(_)), None) => return false,
            (None, Some(VersionPart::Number(o))) => return *o == 0,
            (None, Some(VersionPart::Text(_))) => return true,
            _ => {}
        }
    }
    false
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VersionPart {
    Number(u64),
    Text(String),
}

pub(crate) fn version_parts(version: &str) -> Vec<VersionPart> {
    version
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(|part| {
            part.parse::<u64>()
                .map(VersionPart::Number)
                .unwrap_or_else(|_| VersionPart::Text(part.to_ascii_lowercase()))
        })
        .collect()
}

/// Scan `bytes` (a JSON object) for the value of a top-level string property
/// `key`, without building the whole `Value` tree. Returns `Some(v)` only when
/// the root is an object with a direct `key` whose value is a JSON string;
/// returns `None` otherwise so the caller can fall back to a full parse and stay
/// exactly equivalent to `from_slice(...).get(key).as_str()`.
///
/// Depth-aware: only matches `key` at object depth 1 (the root's own
/// properties), skipping any nested occurrence inside arrays/sub-objects. The
/// value string is decoded via `serde_json` so JSON escapes match a full parse.
fn scan_top_level_string(bytes: &[u8], key: &[u8]) -> Option<String> {
    let mut i = 0usize;
    let n = bytes.len();
    // Skip leading whitespace; the root must be an object `{`.
    while i < n && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= n || bytes[i] != b'{' {
        return None;
    }
    i += 1;
    // depth counts nesting *inside* the root object (0 == direct root props).
    let mut depth: i32 = 0;
    while i < n {
        let c = bytes[i];
        match c {
            b'"' => {
                // A string token. Find its end (respecting escapes).
                let key_start = i + 1;
                let mut j = key_start;
                while j < n {
                    match bytes[j] {
                        b'\\' => j += 2,
                        b'"' => break,
                        _ => j += 1,
                    }
                }
                if j >= n {
                    return None; // unterminated → let full parse decide
                }
                let tok = &bytes[key_start..j];
                i = j + 1;
                if depth == 0 && tok == key {
                    // Expect `: <value>`. Skip ws + colon.
                    while i < n && bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    if i >= n || bytes[i] != b':' {
                        return None;
                    }
                    i += 1;
                    while i < n && bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    // Value must be a JSON string for `as_str()` to be Some.
                    if i >= n || bytes[i] != b'"' {
                        return None;
                    }
                    let val_start = i;
                    let mut k = i + 1;
                    while k < n {
                        match bytes[k] {
                            b'\\' => k += 2,
                            b'"' => break,
                            _ => k += 1,
                        }
                    }
                    if k >= n {
                        return None;
                    }
                    // Decode via serde so escapes match a full parse exactly.
                    let s: String = serde_json::from_slice(&bytes[val_start..=k]).ok()?;
                    return Some(s);
                }
                // Not our key (or nested): continue scanning after this string.
            }
            b'{' | b'[' => {
                depth += 1;
                i += 1;
            }
            b'}' | b']' => {
                if depth == 0 {
                    // End of root object without a matching top-level string.
                    return None;
                }
                depth -= 1;
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

pub(crate) fn probe_name(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    // Byte-scan for the top-level `"name"` string instead of full-parsing the
    // whole SD (which is 10s+ of serde over the corpus, purely to read one
    // field). Falls back to a full parse if the scan cannot find a clean
    // top-level string value, so the result is provably identical to
    // `from_slice(...).get("name")` — the scan only *skips work* on the common
    // fast path. See docs/perf-snapshot-gen.md.
    if let Some(name) = scan_top_level_string(&bytes, b"name") {
        return Some(name);
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod name_scan_tests {
    use super::*;
    use serde_json::Value;

    fn reference(bytes: &[u8]) -> Option<String> {
        let v: Value = serde_json::from_slice(bytes).ok()?;
        v.get("name").and_then(Value::as_str).map(str::to_string)
    }

    fn check(bytes: &[u8]) {
        // scan_top_level_name may return None on rare shapes (deliberate
        // fall-through to full parse); when it returns Some it MUST equal the
        // full-parse result, and probe-equivalent overall.
        if let Some(scanned) = scan_top_level_string(bytes, b"name") {
            assert_eq!(Some(scanned), reference(bytes), "scan disagreed with parse");
        }
    }

    #[test]
    fn synthetic_shapes() {
        // top-level string name
        assert_eq!(
            scan_top_level_string(br#"{"resourceType":"StructureDefinition","name":"Patient"}"#, b"name"),
            Some("Patient".to_string())
        );
        // name after nested objects/arrays
        let s = br#"{"a":{"name":"NESTED"},"b":[{"name":"X"}],"name":"Root"}"#;
        assert_eq!(scan_top_level_string(s, b"name"), Some("Root".to_string()));
        assert_eq!(scan_top_level_string(s, b"name"), reference(s));
        // escaped chars
        let e = br#"{"name":"A\"B\\C\n"}"#;
        assert_eq!(scan_top_level_string(e, b"name"), reference(e));
        // no name
        assert_eq!(scan_top_level_string(br#"{"x":1}"#, b"name"), None);
        // name is not a string -> None (fall back to parse, which is also None)
        assert_eq!(scan_top_level_string(br#"{"name":null}"#, b"name"), None);
        assert_eq!(scan_top_level_string(br#"{"name":123}"#, b"name"), None);
        // only nested name, no top-level -> None
        let only_nested = br#"{"a":{"name":"deep"}}"#;
        assert_eq!(scan_top_level_string(only_nested, b"name"), None);
        assert_eq!(reference(only_nested), None);
        // key that contains "name" as substring must not match
        assert_eq!(scan_top_level_string(br#"{"names":"plural","name":"real"}"#, b"name"), Some("real".to_string()));
        // leading whitespace
        assert_eq!(scan_top_level_string(b"  \n {\"name\":\"W\"}", b"name"), Some("W".to_string()));
        // root is array, not object
        assert_eq!(scan_top_level_string(br#"[{"name":"x"}]"#, b"name"), None);
    }

    #[test]
    fn corpus_equivalence() {
        // Walk the isolated cache and verify scan == parse for every SD file we
        // can find (bounded sample). Skips if the cache is absent.
        let candidates = [
            "temp/fhir-home/.fhir/packages",
            "../../temp/fhir-home/.fhir/packages",
        ];
        let Some(cache) = candidates
            .iter()
            .map(std::path::Path::new)
            .find(|p| p.is_dir())
        else {
            eprintln!("cache absent; skipping corpus_equivalence");
            return;
        };
        let mut checked = 0usize;
        let mut mismatches = 0usize;
        for pkg in std::fs::read_dir(cache).unwrap().flatten() {
            let dir = pkg.path().join("package");
            if !dir.is_dir() {
                continue;
            }
            for f in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = f.path();
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if !name.starts_with("StructureDefinition-") || !name.ends_with(".json") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&p) else { continue };
                let scanned = scan_top_level_string(&bytes, b"name");
                let full = reference(&bytes);
                // Either scan matches full, or scan returned None (fall-through)
                // — in which case probe_name still returns `full` via parse.
                if let Some(s) = &scanned {
                    if Some(s.clone()) != full {
                        mismatches += 1;
                        if mismatches <= 5 {
                            eprintln!("MISMATCH {}: scan={:?} parse={:?}", p.display(), scanned, full);
                        }
                    }
                }
                checked += 1;
                if checked >= 40000 {
                    break;
                }
            }
        }
        eprintln!("corpus_equivalence: checked {checked} SD files, {mismatches} mismatches");
        assert_eq!(mismatches, 0);
        assert!(checked > 1000, "expected to check many files, got {checked}");
    }
}
