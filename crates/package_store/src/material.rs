//! Canonical package material shared by native and in-memory/browser hosts.
//!
//! A FHIR registry tarball may contain nested validation/schema or template
//! payloads, while the compiler's semantic package read model consumes the
//! top-level `package/` files. This module validates the full mounted transport
//! and separately canonicalizes the top-level semantic bytes used by a
//! `SiteBuild` package lock.

use crate::{derived_index, BundleSource};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;

/// Media type for [`NormalizedPackageMaterial::payload`].
pub const NORMALIZED_PACKAGE_MEDIA_TYPE: &str = "application/vnd.fhir.package.normalized.v1";

/// The closed, deterministic representation of one exact FHIR package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NormalizedPackageMaterial {
    /// Every safe file mounted under `<label>/package/`, including nested
    /// template content and a freshly derived top-level sidecar.
    pub files: BTreeMap<String, Vec<u8>>,
    /// Canonical encoding of the compiler-visible top-level files, suitable for
    /// the semantic package lock. Nested template content is transport material
    /// and must become explicit target artifacts before a native-template
    /// `SiteBuild` can claim it as closed input.
    pub payload: Vec<u8>,
    /// Exact dependency requests declared by `package.json`.
    pub declared_dependencies: BTreeMap<String, String>,
}

/// Normalize package-relative entries and validate their declared identity.
///
/// Safe nested members are retained because template packages consume them.
/// Unsafe paths fail instead of being silently mounted outside the synthetic
/// package root. The derived index and lock payload are built from the
/// compiler-visible top-level files, so raw registry and repacked native forms
/// have identical semantic package identity.
pub fn normalize_package_material(
    label: &str,
    entries: BTreeMap<String, Vec<u8>>,
) -> Result<NormalizedPackageMaterial> {
    let (expected_id, expected_version) = parse_exact_label(label)?;
    let mut files = BTreeMap::new();
    for (name, bytes) in entries {
        validate_member_name(&name)?;
        if files.insert(name.clone(), bytes).is_some() {
            bail!("duplicate normalized package member {name:?}");
        }
    }

    let package_json_bytes = files
        .get("package.json")
        .ok_or_else(|| anyhow!("package {label} has no top-level package.json"))?;
    let package_json: Value = serde_json::from_slice(package_json_bytes)
        .with_context(|| format!("parse {label}/package.json"))?;
    let object = package_json
        .as_object()
        .ok_or_else(|| anyhow!("{label}/package.json must be a JSON object"))?;

    let mut saw_id = false;
    for field in ["name", "packageId"] {
        if let Some(value) = object.get(field) {
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("{label}/package.json {field} must be a string"))?;
            saw_id = true;
            if value != expected_id {
                bail!(
                    "mounted package label {label} disagrees with package.json {field}={value:?}"
                );
            }
        }
    }
    if !saw_id {
        bail!("{label}/package.json has no name or packageId");
    }
    let version = object
        .get("version")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{label}/package.json version must be a string"))?;
    if version != expected_version {
        bail!("mounted package label {label} disagrees with package.json version={version:?}");
    }

    let declared_dependencies = match object.get("dependencies") {
        None => BTreeMap::new(),
        Some(value) => serde_json::from_value::<BTreeMap<String, String>>(value.clone())
            .with_context(|| {
                format!("{label}/package.json dependencies must map ids to strings")
            })?,
    };

    // A shipped sidecar is an optimization, not authoritative package input.
    // Rebuilding it makes raw registry and native repacked forms byte-identical.
    files.remove(derived_index::SIDECAR_NAME);
    let mut source = BundleSource::new();
    source.mount_package(label, files.clone());
    let package_dir = source.cache_root().join(label).join("package");
    let index = derived_index::build(&source, &package_dir);
    files.insert(
        derived_index::SIDECAR_NAME.to_string(),
        derived_index::to_bytes(&index),
    );

    let semantic_files = files
        .iter()
        .filter(|(name, _)| !name.contains('/'))
        .map(|(name, bytes)| (name.clone(), bytes.clone()))
        .collect();
    let payload = encode_normalized_package(&semantic_files);
    Ok(NormalizedPackageMaterial {
        files,
        payload,
        declared_dependencies,
    })
}

/// Encode a sorted file map as `(name length, name, body length, body)` tuples.
/// Lengths are unsigned big-endian 64-bit integers. `BTreeMap` supplies the
/// same UTF-8 byte ordering used by Rust's SiteBuild canonical form.
pub fn encode_normalized_package(files: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let capacity = files
        .iter()
        .map(|(name, body)| {
            16usize
                .saturating_add(name.len())
                .saturating_add(body.len())
        })
        .sum();
    let mut encoded = Vec::with_capacity(capacity);
    for (name, body) in files {
        encoded.extend_from_slice(&(name.len() as u64).to_be_bytes());
        encoded.extend_from_slice(name.as_bytes());
        encoded.extend_from_slice(&(body.len() as u64).to_be_bytes());
        encoded.extend_from_slice(body);
    }
    encoded
}

fn parse_exact_label(label: &str) -> Result<(&str, &str)> {
    if label.contains(['/', '\\', '\0']) || label.trim() != label {
        bail!("unsafe package label {label:?}");
    }
    let (id, version) = label
        .split_once('#')
        .ok_or_else(|| anyhow!("package label must be <id>#<version>: {label:?}"))?;
    if id.is_empty() || version.is_empty() || version.contains('#') {
        bail!("package label must contain one non-empty id and version: {label:?}");
    }
    Ok((id, version))
}

/// Validate a package-relative file path before joining it to BundleSource's
/// synthetic root. Nested paths are needed by Publisher template packages.
fn validate_member_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains('\\')
        || name.contains('\0')
        || name
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        bail!("unsafe package member name {name:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package_json(name: &str, version: &str, dependencies: Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "name": name,
            "version": version,
            "dependencies": dependencies,
        }))
        .unwrap()
    }

    #[test]
    fn raw_and_repacked_shapes_have_identical_semantic_identity() {
        let package = package_json(
            "example.pkg",
            "1.2.3",
            serde_json::json!({"dep.pkg": "2.0.0"}),
        );
        let resource = br#"{"resourceType":"Patient","id":"p"}"#.to_vec();
        let raw = normalize_package_material(
            "example.pkg#1.2.3",
            BTreeMap::from([
                ("package.json".into(), package.clone()),
                ("Patient-p.json".into(), resource.clone()),
                ("xml/Patient-p.xml".into(), b"ignored".to_vec()),
            ]),
        )
        .unwrap();
        let repacked = normalize_package_material(
            "example.pkg#1.2.3",
            BTreeMap::from([
                ("package.json".into(), package),
                ("Patient-p.json".into(), resource),
                (
                    derived_index::SIDECAR_NAME.into(),
                    br#"{"derived-index-version":999,"files":[]}"#.to_vec(),
                ),
            ]),
        )
        .unwrap();

        assert_eq!(raw.payload, repacked.payload);
        assert_eq!(raw.declared_dependencies, repacked.declared_dependencies);
        assert!(raw.files.contains_key("xml/Patient-p.xml"));
        assert!(!repacked.files.contains_key("xml/Patient-p.xml"));
        assert_eq!(
            raw.declared_dependencies.get("dep.pkg"),
            Some(&"2.0.0".into())
        );
        assert!(derived_index::parse(&raw.files[derived_index::SIDECAR_NAME]).is_some());
    }

    #[test]
    fn rejects_identity_metadata_and_unsafe_paths() {
        let valid = package_json("example.pkg", "1.2.3", serde_json::json!({}));
        assert!(normalize_package_material(
            "other.pkg#1.2.3",
            BTreeMap::from([("package.json".into(), valid.clone())])
        )
        .is_err());
        assert!(normalize_package_material(
            "example.pkg#1.2.3",
            BTreeMap::from([("../package.json".into(), valid)])
        )
        .is_err());
        assert!(normalize_package_material(
            "example.pkg#1.2.3",
            BTreeMap::from([(
                "package.json".into(),
                package_json("example.pkg", "1.2.3", serde_json::json!([])),
            )])
        )
        .is_err());
    }
}
