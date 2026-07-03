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
            let Ok(json) = std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .ok_or(())
            else {
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

pub(crate) fn probe_name(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
}
