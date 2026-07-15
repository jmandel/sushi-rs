//! Package derived-columns index — the shared format, builder, and reader.
//!
//! A *derived index* lists one row per resource file in a package, carrying the
//! columns that both the acquisition/materialize write side and the read sides
//! (`package_store`, `snapshot_gen`) need but that the stock `.index.json` does
//! not provide — notably `name` and `baseDefinition`. It is derived purely from
//! immutable package *content*, so it is computed once per package content (in
//! the CAS, keyed by content hash + [`DERIVED_INDEX_FORMAT_VERSION`]) and read
//! everywhere else.
//!
//! See `docs/package-derived-index.md` for the full design (CAS artifact
//! lifecycle, sidecar placement, non-CAS write-once fallback).

use crate::source::PackageSource;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

/// Format version of the derived index. Bumping this changes the CAS artifact
/// filename ([`cas_artifact_name`]) so a stale artifact is never read — entries
/// are never mutated in place; a bump invalidates by key. It is intentionally
/// distinct from the stock `.index.json` `index-version` (2).
pub const DERIVED_INDEX_FORMAT_VERSION: u32 = 1;

/// The materialized sidecar filename, written next to `.index.json` in a
/// package directory.
pub const SIDECAR_NAME: &str = ".derived-index.json";

/// The CAS artifact filename (inside `<cas>/packages/<sha256>/derived/`).
pub fn cas_artifact_name() -> String {
    format!("derived-index-v{DERIVED_INDEX_FORMAT_VERSION}.json")
}

/// One derived-index row. Every column is lifted verbatim from the resource's
/// root object (or the directory entry, for `filename`). `None`/absent columns
/// are omitted from the JSON so the artifact stays compact and stable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DerivedEntry {
    pub filename: String,
    #[serde(
        rename = "resourceType",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub resource_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub kind: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none", default)]
    pub sd_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub derivation: Option<String>,
    #[serde(
        rename = "baseDefinition",
        skip_serializing_if = "Option::is_none",
        default
    )]
    pub base_definition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub name: Option<String>,
}

/// The parsed derived index (format version + rows).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DerivedIndex {
    #[serde(rename = "derived-index-version")]
    pub version: u32,
    pub files: Vec<DerivedEntry>,
}

/// The set of resource files a package exposes, in FPL `getPotentialResourcePaths`
/// order: every top-level `^[^.].*\.json$` except `package.json`, sorted. This is
/// content-derived, so it also covers packages whose stock `.index.json` is
/// empty or SD-less.
fn resource_filenames(source: &dyn PackageSource, package_dir: &Path) -> Vec<String> {
    let Ok(rd) = source.read_dir(package_dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = Vec::new();
    for ent in rd {
        if !ent.is_file {
            continue;
        }
        let name = ent.file_name;
        if name.starts_with('.') || !name.to_ascii_lowercase().ends_with(".json") {
            continue;
        }
        if name == "package.json" {
            continue;
        }
        files.push(name);
    }
    files.sort();
    files
}

fn entry_from_json(json: &Value, filename: String) -> DerivedEntry {
    let s = |k: &str| json.get(k).and_then(Value::as_str).map(str::to_string);
    DerivedEntry {
        filename,
        resource_type: s("resourceType"),
        id: s("id"),
        url: s("url"),
        version: s("version"),
        kind: s("kind"),
        sd_type: s("type"),
        derivation: s("derivation"),
        base_definition: s("baseDefinition"),
        name: s("name"),
    }
}

fn entry_from_bytes(bytes: &[u8], filename: String) -> Option<DerivedEntry> {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .map(|json| entry_from_json(&json, filename))
}

/// Build the derived index for a materialized package directory by parsing each
/// resource file once and lifting the derived columns from its root object. This
/// is the single builder that both the CAS ingest write and the non-CAS
/// write-once fallback use, so their outputs are byte-identical.
pub fn build(source: &dyn PackageSource, package_dir: &Path) -> DerivedIndex {
    let mut files = Vec::new();
    for filename in resource_filenames(source, package_dir) {
        let path = package_dir.join(&filename);
        let Some(entry) = source
            .read(&path)
            .ok()
            .and_then(|bytes| entry_from_bytes(&bytes, filename))
        else {
            // A file FPL would read then reject (unparseable): skip it, exactly
            // like the stock index builder.
            continue;
        };
        files.push(entry);
    }
    DerivedIndex {
        version: DERIVED_INDEX_FORMAT_VERSION,
        files,
    }
}

/// Build the same derived index directly from canonical package members.
///
/// Callers which already own the exact package map need not clone it into a
/// temporary `PackageSource` merely to read the top-level JSON files back.
/// Nested members, dotfiles, and `package.json` remain excluded exactly as they
/// are by [`build`].
pub(crate) fn build_from_package_files(package_files: &BTreeMap<String, Vec<u8>>) -> DerivedIndex {
    let files = package_files
        .iter()
        .filter(|(filename, _)| {
            !filename.contains('/')
                && !filename.starts_with('.')
                && filename.to_ascii_lowercase().ends_with(".json")
                && filename.as_str() != "package.json"
        })
        .filter_map(|(filename, bytes)| entry_from_bytes(bytes, filename.clone()))
        .collect();
    DerivedIndex {
        version: DERIVED_INDEX_FORMAT_VERSION,
        files,
    }
}

/// Serialize a derived index to compact JSON bytes (the exact bytes written to
/// the CAS artifact and the sidecar).
pub fn to_bytes(index: &DerivedIndex) -> Vec<u8> {
    serde_json::to_vec(index).expect("DerivedIndex serializes")
}

/// Parse a derived-index sidecar/artifact, returning its rows only if the format
/// version matches [`DERIVED_INDEX_FORMAT_VERSION`] (a version mismatch is
/// treated as absent so the caller rebuilds/falls back).
pub fn parse(bytes: &[u8]) -> Option<Vec<DerivedEntry>> {
    let idx: DerivedIndex = serde_json::from_slice(bytes).ok()?;
    if idx.version != DERIVED_INDEX_FORMAT_VERSION {
        return None;
    }
    Some(idx.files)
}

/// Load the derived index for a materialized package directory.
///
/// 1. If the `.derived-index.json` sidecar is present and current, read it.
/// 2. Otherwise derive it from package content, **write it once** next to
///    `.index.json`, and return it. This covers non-CAS caches (plain extracted
///    dirs, the already-materialized isolated test cache).
/// 3. If the sidecar cannot be written (read-only dir, e.g. a symlink into
///    read-only CAS content), still return the freshly-built rows — the caller
///    gets correct data and simply pays the one-process build cost. Fail-loud
///    safe: never returns wrong data, never errors.
pub fn load(source: &dyn PackageSource, package_dir: &Path) -> Vec<DerivedEntry> {
    let sidecar = package_dir.join(SIDECAR_NAME);
    if let Ok(bytes) = source.read(&sidecar) {
        if let Some(rows) = parse(&bytes) {
            return rows;
        }
    }
    let index = build(source, package_dir);
    let bytes = to_bytes(&index);
    // Write-once via the source. Writable (disk) sources create the sidecar
    // atomically; read-only sources (bundle, CAS symlink) return Err and we
    // fail-soft — the freshly built rows are already correct in memory.
    let _ = source.write_new(&sidecar, &bytes);
    index.files
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{source::DiskSource, BundleSource};

    #[test]
    fn build_covers_content_including_name_and_base() {
        let dir = std::env::temp_dir().join(format!("derived_build_{}", std::process::id()));
        let pkg = dir.join("package");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(
            pkg.join("StructureDefinition-foo.json"),
            r#"{"resourceType":"StructureDefinition","id":"foo","url":"http://x/foo","version":"1.0.0","name":"Foo","kind":"resource","type":"Patient","derivation":"constraint","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Patient"}"#,
        )
        .unwrap();
        std::fs::write(pkg.join("package.json"), r#"{"name":"p"}"#).unwrap();
        std::fs::write(pkg.join(".index.json"), r#"{"files":[]}"#).unwrap();

        let idx = build(&DiskSource, &pkg);
        assert_eq!(idx.version, DERIVED_INDEX_FORMAT_VERSION);
        assert_eq!(idx.files.len(), 1, "package.json and dotfiles excluded");
        let e = &idx.files[0];
        assert_eq!(e.filename, "StructureDefinition-foo.json");
        assert_eq!(e.name.as_deref(), Some("Foo"));
        assert_eq!(
            e.base_definition.as_deref(),
            Some("http://hl7.org/fhir/StructureDefinition/Patient")
        );
        assert_eq!(e.sd_type.as_deref(), Some("Patient"));
        assert_eq!(e.derivation.as_deref(), Some("constraint"));

        // load() writes the sidecar once, then reads it back identically.
        let via_load = load(&DiskSource, &pkg);
        assert_eq!(via_load, idx.files);
        assert!(pkg.join(SIDECAR_NAME).is_file());
        // second load reads the sidecar (same rows).
        assert_eq!(load(&DiskSource, &pkg), idx.files);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_rejects_wrong_version() {
        let bytes = br#"{"derived-index-version":999,"files":[]}"#;
        assert!(parse(bytes).is_none());
        let ok = br#"{"derived-index-version":1,"files":[]}"#;
        assert_eq!(parse(ok), Some(Vec::new()));
    }

    #[test]
    fn borrowed_package_map_builder_matches_package_source_builder() {
        let package_files = BTreeMap::from([
            (
                "StructureDefinition-foo.json".into(),
                br#"{"resourceType":"StructureDefinition","id":"foo","name":"Foo"}"#.to_vec(),
            ),
            ("Observation-invalid.json".into(), b"not json".to_vec()),
            ("package.json".into(), br#"{"name":"example.pkg"}"#.to_vec()),
            (".index.json".into(), br#"{"files":[]}"#.to_vec()),
            (
                "nested/Patient-p.json".into(),
                br#"{"resourceType":"Patient","id":"nested"}"#.to_vec(),
            ),
        ]);
        let borrowed = build_from_package_files(&package_files);
        let mut source = BundleSource::new();
        source.mount_package("example.pkg#1.0.0", package_files);
        let package_dir = source
            .cache_root()
            .join("example.pkg#1.0.0")
            .join("package");
        let mounted = build(&source, &package_dir);

        assert_eq!(to_bytes(&borrowed), to_bytes(&mounted));
    }
}
