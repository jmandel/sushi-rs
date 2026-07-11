//! Renderer-neutral semantic result of FHIR guide preparation.
//!
//! This crate deliberately does not depend on `site_build`, so the
//! same prepared value can feed renderer manifests and optional relational
//! compatibility projections without reversing the dependency direction.

pub mod augment;
pub mod native;
pub mod semantics;
pub mod timefmt;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PreparedPath(String);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("prepared path must be a normalized, non-empty relative POSIX path")]
pub struct PreparedPathError;

impl PreparedPath {
    pub fn parse(value: impl Into<String>) -> Result<Self, PreparedPathError> {
        let value = value.into();
        let valid = !value.is_empty()
            && !value.starts_with('/')
            && !value.contains('\\')
            && !value.contains('\0')
            && value
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..");
        valid.then_some(Self(value)).ok_or(PreparedPathError)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PreparedPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for PreparedPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PreparedPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticResourceKey {
    pub resource_type: String,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GeneratedIdentity {
    pub epoch_seconds: i64,
    pub date: String,
    pub day: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceControlIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GuideIdentity {
    pub implementation_guide: SemanticResourceKey,
    pub package_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub fhir_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_label: Option<String>,
    pub fhir_publication_base: String,
    pub generated: GeneratedIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_control: Option<SourceControlIdentity>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PublisherCompatibility {
    pub error_count: String,
    pub tooling_version: String,
    pub tooling_revision: String,
    pub tooling_version_full: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResourcePublication {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standard_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_definition: Option<String>,
}

impl ResourcePublication {
    pub fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.description.is_none()
            && self.standard_status.is_none()
            && self.base_definition.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SemanticResource {
    pub key: SemanticResourceKey,
    pub resource: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication: Option<ResourcePublication>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExpansionCode {
    pub system: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ValueSetExpansion {
    pub value_set: SemanticResourceKey,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub codes: Vec<ExpansionCode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PageNode {
    pub name_url: String,
    pub title: String,
    pub generation: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Exact authored project source for `body`. Generated navigation nodes
    /// have neither a body nor a source.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<PreparedPath>,
    pub children: Vec<PageNode>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MenuNode {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    pub items: Vec<MenuNode>,
}

/// Publisher-facing role of a captured authored file. Paths are relative to
/// the role's declared root, while `source_reads` retain exact project
/// ownership. Roles are deliberately distinct: an include or `_data` input is
/// renderer input, not a public site asset.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AuthoredFileRole {
    PageContent,
    ResourceContent,
    Data,
    Include,
    Image,
    ImageSource,
}

/// A captured authored Publisher input and its exact project source.
///
/// `role` prevents downstream projections from treating every captured input
/// as a public asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthoredFile {
    pub role: AuthoredFileRole,
    pub path: PreparedPath,
    pub mime: String,
    pub content: Vec<u8>,
    pub source_reads: BTreeSet<PreparedPath>,
}

/// Renderer-neutral, in-memory guide semantics.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedGuide {
    pub guide: GuideIdentity,
    pub resources: Vec<SemanticResource>,
    pub publisher_compatibility: Option<PublisherCompatibility>,
    pub expansions: Vec<ValueSetExpansion>,
    pub pages: Vec<PageNode>,
    pub menu: Vec<MenuNode>,
    pub sushi_config: Value,
    pub authored_files: Vec<AuthoredFile>,
}

/// Byte source for authored guide inputs. This is part of PreparedGuide
/// preparation, not a relational database concern; native and browser hosts
/// provide the same normalized paths through different implementations.
pub trait FileSource {
    fn read(&self, path: &Path) -> Option<Vec<u8>>;

    fn is_file(&self, path: &Path) -> bool {
        self.read(path).is_some()
    }

    /// Recursively list files under `dir` as sorted relative POSIX paths.
    fn list_recursive(&self, dir: &Path) -> Vec<String>;
}

/// Native authored-input source.
pub struct DiskFiles;

impl FileSource for DiskFiles {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        std::fs::read(path).ok()
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn list_recursive(&self, dir: &Path) -> Vec<String> {
        fn walk(root: &Path, current: &Path, out: &mut Vec<String>) {
            let Ok(entries) = std::fs::read_dir(current) else {
                return;
            };
            let mut entries: Vec<_> = entries.filter_map(Result::ok).collect();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() {
                    walk(root, &path, out);
                } else if kind.is_file() {
                    if let Ok(relative) = path.strip_prefix(root) {
                        out.push(relative.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }

        let mut out = Vec::new();
        walk(dir, dir, &mut out);
        out.sort();
        out
    }
}

/// Browser/in-memory authored-input source.
pub struct MemFiles {
    files: BTreeMap<PathBuf, Vec<u8>>,
}

impl MemFiles {
    pub fn new(files: BTreeMap<PathBuf, Vec<u8>>) -> Self {
        Self { files }
    }
}

impl FileSource for MemFiles {
    fn read(&self, path: &Path) -> Option<Vec<u8>> {
        self.files.get(path).cloned()
    }

    fn is_file(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    fn list_recursive(&self, dir: &Path) -> Vec<String> {
        let mut out = self
            .files
            .keys()
            .filter_map(|path| path.strip_prefix(dir).ok())
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .filter(|relative| !relative.is_empty())
            .collect::<Vec<_>>();
        out.sort();
        out
    }
}

/// Renderer-neutral authored augmentation inputs.
pub struct AugmentInputs<'a> {
    pub ig: &'a Value,
    pub sushi_config_yaml: &'a str,
    pub project_root: PathBuf,
    pub pagecontent_dir: PathBuf,
    pub image_dir: PathBuf,
    pub liquid_asset_dirs: Vec<PathBuf>,
    pub files: &'a dyn FileSource,
}

/// Renderer-neutral authored navigation/config/files captured during guide
/// preparation. Relational compatibility rows, when requested, are projected
/// only after this value is complete.
pub struct PreparedAugmentation {
    pub pages: Vec<PageNode>,
    pub menu: Vec<MenuNode>,
    pub sushi_config: Value,
    pub authored_files: Vec<AuthoredFile>,
}
