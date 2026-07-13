use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::{
    canonical_json_bytes, sha256_canonical, BuildId, CanonicalError, ContentRef, Sha256Digest,
};

/// Required media type for every package carrier rooted by SiteBuild v2.
pub const PREPARED_PACKAGE_MEDIA_TYPE: &str = "application/vnd.fhir.package.prepared.v3";

/// Wire-format discriminator. A new incompatible contract requires a new enum
/// variant rather than silently changing existing hashing semantics.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
pub enum SchemaVersion {
    #[serde(rename = "site-build/v2")]
    V2,
}

/// Canonical project-relative path. Paths are POSIX-style even on native hosts.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(type = "string"))]
#[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
pub struct SourcePath(String);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SourcePathError {
    #[error("source path must be a normalized, non-empty relative POSIX path")]
    Invalid,
}

impl SourcePath {
    pub fn parse(value: impl Into<String>) -> Result<Self, SourcePathError> {
        let value = value.into();
        let valid = !value.is_empty()
            && !value.starts_with('/')
            && !value.contains('\\')
            && !value.contains('\0')
            && value
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..");
        if !valid {
            return Err(SourcePathError::Invalid);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourcePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for SourcePath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SourcePath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceKind {
    Fsh,
    Config,
    PredefinedResource,
    Page,
    Template,
    Asset,
    Other { name: String },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SourceEntry {
    pub kind: SourceKind,
    pub content: ContentRef,
}

/// Exact source bytes that define a project revision.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "wire-contract",
    ts(type = "{ [key in string]: SourceEntry }")
)]
#[cfg_attr(
    feature = "wire-contract",
    schemars(with = "BTreeMap<String, SourceEntry>")
)]
#[serde(transparent)]
pub struct SourceManifest(BTreeMap<SourcePath, SourceEntry>);

impl SourceManifest {
    pub fn from_entries(
        entries: impl IntoIterator<Item = (SourcePath, SourceEntry)>,
    ) -> Result<Self, ContractError> {
        let mut result = BTreeMap::new();
        for (path, entry) in entries {
            if result.insert(path.clone(), entry).is_some() {
                return Err(ContractError::DuplicateSource(path));
            }
        }
        Ok(Self(result))
    }

    pub fn get(&self, path: &SourcePath) -> Option<&SourceEntry> {
        self.0.get(path)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SourcePath, &SourceEntry)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProjectIdentity {
    pub project_id: String,
    /// An SCM revision when available; otherwise a caller-defined immutable
    /// revision label. Exact source hashes remain authoritative.
    pub revision: String,
    pub sources: SourceManifest,
}

/// An exact FHIR package coordinate (`package.id#version`).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(type = "string"))]
#[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
pub struct PackageCoordinate(String);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum PackageCoordinateError {
    #[error("package coordinate must be an exact non-wildcard 'package.id#version'")]
    Invalid,
}

impl PackageCoordinate {
    pub fn new(package_id: &str, version: &str) -> Result<Self, PackageCoordinateError> {
        let lower = version.to_ascii_lowercase();
        let exact = package_id == package_id.trim()
            && version == version.trim()
            && !package_id.is_empty()
            && !package_id.contains('#')
            && !package_id.chars().any(|ch| matches!(ch, '/' | '\\' | '\0'))
            && !package_id.chars().any(char::is_whitespace)
            && !version.is_empty()
            && !version.contains('#')
            && !version.chars().any(|ch| {
                matches!(
                    ch,
                    '/' | '\\' | '\0' | '^' | '~' | '<' | '>' | '=' | '|' | ','
                )
            })
            && !version.chars().any(char::is_whitespace)
            && lower != "latest"
            && lower != "current"
            && lower != "dev"
            && !version.contains('*')
            && !version
                .split('.')
                .any(|part| part.eq_ignore_ascii_case("x"));
        if !exact {
            return Err(PackageCoordinateError::Invalid);
        }
        Ok(Self(format!("{package_id}#{version}")))
    }

    pub fn parse(value: &str) -> Result<Self, PackageCoordinateError> {
        let (id, version) = value
            .split_once('#')
            .ok_or(PackageCoordinateError::Invalid)?;
        Self::new(id, version)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn package_id(&self) -> &str {
        self.0.split_once('#').expect("validated coordinate").0
    }

    pub fn version(&self) -> &str {
        self.0.split_once('#').expect("validated coordinate").1
    }
}

impl fmt::Display for PackageCoordinate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for PackageCoordinate {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PackageCoordinate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct LockedPackage {
    pub coordinate: PackageCoordinate,
    /// Exact deterministic prepared-package carrier. This content-addressed
    /// value binds the complete package representation consumed by execution,
    /// not merely its registry name/version or a normalized semantic subset.
    pub content: ContentRef,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub dependencies: BTreeSet<PackageCoordinate>,
}

/// Exact package closure. Keys are repeated in each value so a key/value mismatch
/// cannot be hidden by a serialization adapter.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(
    feature = "wire-contract",
    ts(type = "{ [key in string]: LockedPackage }")
)]
#[cfg_attr(
    feature = "wire-contract",
    schemars(with = "BTreeMap<String, LockedPackage>")
)]
#[serde(transparent)]
pub struct PackageLock(BTreeMap<PackageCoordinate, LockedPackage>);

impl PackageLock {
    pub fn from_packages(
        packages: impl IntoIterator<Item = LockedPackage>,
    ) -> Result<Self, ContractError> {
        let mut result = BTreeMap::new();
        for package in packages {
            let coordinate = package.coordinate.clone();
            if result.insert(coordinate.clone(), package).is_some() {
                return Err(ContractError::DuplicatePackage(coordinate));
            }
        }
        Ok(Self(result))
    }

    pub fn get(&self, coordinate: &PackageCoordinate) -> Option<&LockedPackage> {
        self.0.get(coordinate)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PackageCoordinate, &LockedPackage)> {
        self.0.iter()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ProducerRef {
    pub id: String,
    pub version: String,
}

impl ProducerRef {
    pub fn new(id: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            version: version.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum RenderMode {
    NativeTemplate,
    ExternalBuilder,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RenderTarget {
    pub renderer: ProducerRef,
    pub mode: RenderMode,
    pub fhir_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", ts(optional))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "PackageCoordinate"))]
    pub template: Option<PackageCoordinate>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parameters: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ResourceKey {
    pub resource_type: String,
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FragmentKind {
    Narrative,
    Summary,
    Dictionary,
    Terminology,
    Table,
    Other { name: String },
}

/// The subject of a generated fragment. Some Publisher includes describe the
/// guide as a whole (for example dependency/summary material) rather than one
/// resource, so resource scope must not be fabricated for them.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FragmentScope {
    WholeIg,
    Resource { resource: ResourceKey },
}

/// Asset ownership is part of identity. Identical relative paths from a template,
/// authored input, Publisher runtime, and generator are distinct artifacts until
/// an explicit assembly policy chooses which one is served.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AssetNamespace {
    Authored,
    Template,
    PublisherRuntime,
    Generated,
    Other { name: String },
}

/// Typed identity of one semantic or rendered artifact.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArtifactKey {
    SemanticModel {
        name: String,
    },
    Resource {
        resource: ResourceKey,
    },
    Fragment {
        scope: FragmentScope,
        fragment: FragmentKind,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        parameters: BTreeMap<String, String>,
    },
    Page {
        path: SourcePath,
    },
    Asset {
        namespace: AssetNamespace,
        path: SourcePath,
    },
    Data {
        namespace: String,
        name: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadDependency {
    Source { path: SourcePath },
    Package { coordinate: PackageCoordinate },
    Artifact { key: ArtifactKey },
    Content { sha256: Sha256Digest },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ArtifactProvenance {
    pub producer: ProducerRef,
    pub recipe: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Information,
    Warning,
    Error,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SourceLocation {
    pub path: SourcePath,
    /// One-based, matching SUSHI's source line reporting.
    pub line: u32,
    /// Zero-based, matching the parser/ANTLR column convention.
    pub column: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BuildDiagnostic {
    /// Stable producer-assigned emission order. Keeping it in the value preserves
    /// meaningful diagnostic order while making collection insertion irrelevant.
    #[serde(default)]
    pub sequence: u64,
    pub severity: DiagnosticSeverity,
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", ts(optional))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "SourceLocation"))]
    pub location: Option<SourceLocation>,
}

impl BuildDiagnostic {
    pub fn new(
        severity: DiagnosticSeverity,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            sequence: 0,
            severity,
            code: code.into(),
            message: message.into(),
            location: None,
        }
    }

    pub fn with_sequence(mut self, sequence: u64) -> Self {
        self.sequence = sequence;
        self
    }
}

/// State is exhaustive on purpose. There is no absent/null state that a consumer
/// could accidentally interpret as either "not requested" or "failed".
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ArtifactState {
    Ready {
        content: ContentRef,
    },
    Deferred {
        reason: String,
    },
    Unsupported {
        capability: String,
        reason: String,
    },
    Failed {
        diagnostics: BTreeSet<BuildDiagnostic>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRecord {
    pub key: ArtifactKey,
    pub state: ArtifactState,
    pub provenance: ArtifactProvenance,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub reads: BTreeSet<ReadDependency>,
}

/// Sorted typed artifact catalog. The wire shape is a list because complex enum
/// keys cannot be represented faithfully as JSON object property names.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(type = "Array<ArtifactRecord>"))]
#[cfg_attr(feature = "wire-contract", schemars(with = "Vec<ArtifactRecord>"))]
pub struct ArtifactCatalog(BTreeMap<ArtifactKey, ArtifactRecord>);

impl ArtifactCatalog {
    pub fn from_records(
        records: impl IntoIterator<Item = ArtifactRecord>,
    ) -> Result<Self, ContractError> {
        let mut result = BTreeMap::new();
        for record in records {
            let key = record.key.clone();
            if result.insert(key.clone(), record).is_some() {
                return Err(ContractError::DuplicateArtifact(Box::new(key)));
            }
        }
        Ok(Self(result))
    }

    pub fn get(&self, key: &ArtifactKey) -> Option<&ArtifactRecord> {
        self.0.get(key)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&ArtifactKey, &ArtifactRecord)> {
        self.0.iter()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Artifacts a renderer promises it can consume without discovering undeclared
/// requirements. Sealing also follows their typed artifact read dependencies,
/// so this set contains roots rather than requiring callers to duplicate the
/// transitive closure manually.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct RenderPlan {
    required_artifacts: BTreeSet<ArtifactKey>,
}

impl RenderPlan {
    pub fn new(required_artifacts: impl IntoIterator<Item = ArtifactKey>) -> Self {
        Self {
            required_artifacts: required_artifacts.into_iter().collect(),
        }
    }

    pub fn required_artifacts(&self) -> &BTreeSet<ArtifactKey> {
        &self.required_artifacts
    }

    pub fn is_empty(&self) -> bool {
        self.required_artifacts.is_empty()
    }
}

impl Serialize for ArtifactCatalog {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.values().collect::<Vec<_>>().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ArtifactCatalog {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let records = Vec::<ArtifactRecord>::deserialize(deserializer)?;
        Self::from_records(records).map_err(de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ContractError {
    #[error("duplicate source path {0}")]
    DuplicateSource(SourcePath),
    #[error("duplicate package {0}")]
    DuplicatePackage(PackageCoordinate),
    #[error("package {package} has unsupported carrier media type {media_type:?}")]
    UnsupportedPackageMediaType {
        package: PackageCoordinate,
        media_type: Option<String>,
    },
    #[error("package lock key {key} does not match embedded coordinate {embedded}")]
    PackageKeyMismatch {
        key: PackageCoordinate,
        embedded: PackageCoordinate,
    },
    #[error("duplicate artifact {0:?}")]
    DuplicateArtifact(Box<ArtifactKey>),
    #[error("project_id, revision, renderer id/version, and FHIR version must be non-empty")]
    EmptyIdentity,
    #[error("render target references package not present in the exact lock: {0}")]
    MissingTemplatePackage(PackageCoordinate),
    #[error("locked package {package} depends on package absent from the lock: {dependency}")]
    MissingPackageDependency {
        package: PackageCoordinate,
        dependency: PackageCoordinate,
    },
    #[error("artifact {artifact:?} reads missing source {source_path}")]
    MissingSourceDependency {
        artifact: Box<ArtifactKey>,
        source_path: SourcePath,
    },
    #[error("artifact {artifact:?} reads package absent from the lock: {package}")]
    MissingArtifactPackageDependency {
        artifact: Box<ArtifactKey>,
        package: PackageCoordinate,
    },
    #[error("artifact {artifact:?} reads artifact absent from the catalog: {dependency:?}")]
    MissingArtifactDependency {
        artifact: Box<ArtifactKey>,
        dependency: Box<ArtifactKey>,
    },
    #[error("artifact {artifact:?} has an invalid state: {reason}")]
    InvalidArtifactState {
        artifact: Box<ArtifactKey>,
        reason: String,
    },
    #[error("artifact key has an empty identifier: {0:?}")]
    InvalidArtifactKey(Box<ArtifactKey>),
    #[error("render plan requires artifact absent from the catalog: {0:?}")]
    MissingRequiredArtifact(Box<ArtifactKey>),
    #[error("diagnostic code/message must be non-empty and location lines are one-based")]
    InvalidDiagnostic,
    #[error("content media type, when present, must be non-empty")]
    InvalidContentRef,
    #[error("producer id/version and provenance recipe must be non-empty")]
    InvalidProvenance,
}

#[derive(Debug, Error)]
pub enum SiteBuildError {
    #[error(transparent)]
    Contract(#[from] ContractError),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error("build id mismatch: document has {actual}, canonical content requires {expected}")]
    BuildIdMismatch { actual: BuildId, expected: BuildId },
}

/// Why one artifact prevents callback-free consumption of a render plan.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SealBlocker {
    Missing {
        key: ArtifactKey,
    },
    Deferred {
        key: ArtifactKey,
        reason: String,
    },
    Unsupported {
        key: ArtifactKey,
        capability: String,
        reason: String,
    },
    Failed {
        key: ArtifactKey,
        diagnostics: BTreeSet<BuildDiagnostic>,
    },
}

/// Deterministic report of every non-ready artifact reachable from a render
/// plan's required roots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealError {
    blockers: Vec<SealBlocker>,
}

impl SealError {
    pub fn blockers(&self) -> &[SealBlocker] {
        &self.blockers
    }
}

impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "site build is not closed: {} required artifact(s) are not ready",
            self.blockers.len()
        )
    }
}

impl std::error::Error for SealError {}

/// Immutable v2 build handoff. Fields are private so mutation cannot invalidate
/// the content-derived `build_id`; create a new value when an artifact resolves.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct SiteBuild {
    schema_version: SchemaVersion,
    build_id: BuildId,
    project: ProjectIdentity,
    package_lock: PackageLock,
    render_target: RenderTarget,
    render_plan: RenderPlan,
    artifacts: ArtifactCatalog,
    diagnostics: BTreeSet<BuildDiagnostic>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HashPayload<'a> {
    schema_version: SchemaVersion,
    project: &'a ProjectIdentity,
    package_lock: &'a PackageLock,
    render_target: &'a RenderTarget,
    render_plan: &'a RenderPlan,
    artifacts: &'a ArtifactCatalog,
    diagnostics: &'a BTreeSet<BuildDiagnostic>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SiteBuildWire {
    schema_version: SchemaVersion,
    build_id: BuildId,
    project: ProjectIdentity,
    package_lock: PackageLock,
    render_target: RenderTarget,
    render_plan: RenderPlan,
    artifacts: ArtifactCatalog,
    diagnostics: BTreeSet<BuildDiagnostic>,
}

impl SiteBuild {
    pub fn new(
        project: ProjectIdentity,
        package_lock: PackageLock,
        render_target: RenderTarget,
        render_plan: RenderPlan,
        artifacts: ArtifactCatalog,
        diagnostics: BTreeSet<BuildDiagnostic>,
    ) -> Result<Self, SiteBuildError> {
        let mut build = Self {
            schema_version: SchemaVersion::V2,
            build_id: BuildId::from_digest(Sha256Digest::of_bytes(&[])),
            project,
            package_lock,
            render_target,
            render_plan,
            artifacts,
            diagnostics,
        };
        build.validate_contract()?;
        build.build_id = build.expected_build_id()?;
        Ok(build)
    }

    pub fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }

    pub fn build_id(&self) -> &BuildId {
        &self.build_id
    }

    pub fn project(&self) -> &ProjectIdentity {
        &self.project
    }

    pub fn package_lock(&self) -> &PackageLock {
        &self.package_lock
    }

    pub fn render_target(&self) -> &RenderTarget {
        &self.render_target
    }

    pub fn render_plan(&self) -> &RenderPlan {
        &self.render_plan
    }

    pub fn artifacts(&self) -> &ArtifactCatalog {
        &self.artifacts
    }

    pub fn diagnostics(&self) -> &BTreeSet<BuildDiagnostic> {
        &self.diagnostics
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_json_bytes(self)
    }

    /// Recheck both referential integrity and the content-derived id. Useful at
    /// trust boundaries even though deserialization already performs this check.
    pub fn verify(&self) -> Result<(), SiteBuildError> {
        self.validate_contract()?;
        let expected = self.expected_build_id()?;
        if self.build_id != expected {
            return Err(SiteBuildError::BuildIdMismatch {
                actual: self.build_id.clone(),
                expected,
            });
        }
        Ok(())
    }

    /// Consume and seal this build for a callback-free renderer. All required
    /// roots and every artifact they read transitively must already be `Ready`.
    pub fn close(self) -> Result<ClosedSiteBuild, SealError> {
        ClosedSiteBuild::try_from(self)
    }

    fn expected_build_id(&self) -> Result<BuildId, CanonicalError> {
        Ok(BuildId::from_digest(sha256_canonical(&HashPayload {
            schema_version: self.schema_version,
            project: &self.project,
            package_lock: &self.package_lock,
            render_target: &self.render_target,
            render_plan: &self.render_plan,
            artifacts: &self.artifacts,
            diagnostics: &self.diagnostics,
        })?))
    }

    fn validate_contract(&self) -> Result<(), ContractError> {
        if self.project.project_id.trim().is_empty()
            || self.project.revision.trim().is_empty()
            || self.render_target.renderer.id.trim().is_empty()
            || self.render_target.renderer.version.trim().is_empty()
            || self.render_target.fhir_version.trim().is_empty()
        {
            return Err(ContractError::EmptyIdentity);
        }
        if let Some(template) = &self.render_target.template {
            if self.package_lock.get(template).is_none() {
                return Err(ContractError::MissingTemplatePackage(template.clone()));
            }
        }
        for (coordinate, package) in self.package_lock.iter() {
            if coordinate != &package.coordinate {
                return Err(ContractError::PackageKeyMismatch {
                    key: coordinate.clone(),
                    embedded: package.coordinate.clone(),
                });
            }
            validate_content(&package.content)?;
            if package.content.media_type.as_deref() != Some(PREPARED_PACKAGE_MEDIA_TYPE) {
                return Err(ContractError::UnsupportedPackageMediaType {
                    package: coordinate.clone(),
                    media_type: package.content.media_type.clone(),
                });
            }
            for dependency in &package.dependencies {
                if self.package_lock.get(dependency).is_none() {
                    return Err(ContractError::MissingPackageDependency {
                        package: coordinate.clone(),
                        dependency: dependency.clone(),
                    });
                }
            }
        }
        for (_, source) in self.project.sources.iter() {
            validate_content(&source.content)?;
        }
        for diagnostic in &self.diagnostics {
            validate_diagnostic(diagnostic)?;
        }
        for key in self.render_plan.required_artifacts() {
            validate_artifact_key(key)?;
            if self.artifacts.get(key).is_none() {
                return Err(ContractError::MissingRequiredArtifact(Box::new(
                    key.clone(),
                )));
            }
        }
        for (key, record) in self.artifacts.iter() {
            validate_artifact_key(key)?;
            if record.provenance.producer.id.trim().is_empty()
                || record.provenance.producer.version.trim().is_empty()
                || record.provenance.recipe.trim().is_empty()
            {
                return Err(ContractError::InvalidProvenance);
            }
            match &record.state {
                ArtifactState::Ready { content } => validate_content(content)?,
                ArtifactState::Deferred { reason } if reason.trim().is_empty() => {
                    return Err(ContractError::InvalidArtifactState {
                        artifact: Box::new(key.clone()),
                        reason: "deferred reason is empty".into(),
                    });
                }
                ArtifactState::Unsupported { capability, reason }
                    if capability.trim().is_empty() || reason.trim().is_empty() =>
                {
                    return Err(ContractError::InvalidArtifactState {
                        artifact: Box::new(key.clone()),
                        reason: "unsupported capability/reason is empty".into(),
                    });
                }
                ArtifactState::Failed { diagnostics } if diagnostics.is_empty() => {
                    return Err(ContractError::InvalidArtifactState {
                        artifact: Box::new(key.clone()),
                        reason: "failed state has no diagnostics".into(),
                    });
                }
                ArtifactState::Failed { diagnostics } => {
                    for diagnostic in diagnostics {
                        validate_diagnostic(diagnostic)?;
                    }
                }
                _ => {}
            }
            for dependency in &record.reads {
                match dependency {
                    ReadDependency::Source { path } if self.project.sources.get(path).is_none() => {
                        return Err(ContractError::MissingSourceDependency {
                            artifact: Box::new(key.clone()),
                            source_path: path.clone(),
                        });
                    }
                    ReadDependency::Package { coordinate }
                        if self.package_lock.get(coordinate).is_none() =>
                    {
                        return Err(ContractError::MissingArtifactPackageDependency {
                            artifact: Box::new(key.clone()),
                            package: coordinate.clone(),
                        });
                    }
                    ReadDependency::Artifact { key: dependency }
                        if self.artifacts.get(dependency).is_none() =>
                    {
                        return Err(ContractError::MissingArtifactDependency {
                            artifact: Box::new(key.clone()),
                            dependency: Box::new(dependency.clone()),
                        });
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

/// Proof that a [`SiteBuild`]'s render-plan closure is fully materialized. This
/// wrapper is the value an external builder should accept when callbacks are not
/// part of its architecture.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(type = "SiteBuild"))]
#[cfg_attr(feature = "wire-contract", schemars(with = "SiteBuild"))]
#[serde(transparent)]
pub struct ClosedSiteBuild(SiteBuild);

impl ClosedSiteBuild {
    pub fn site_build(&self) -> &SiteBuild {
        &self.0
    }

    pub fn into_site_build(self) -> SiteBuild {
        self.0
    }
}

impl TryFrom<SiteBuild> for ClosedSiteBuild {
    type Error = SealError;

    fn try_from(build: SiteBuild) -> Result<Self, Self::Error> {
        let mut pending = build.render_plan.required_artifacts.clone();
        let mut visited = BTreeSet::new();
        let mut blockers = Vec::new();

        while let Some(key) = pending.pop_first() {
            if !visited.insert(key.clone()) {
                continue;
            }
            let Some(record) = build.artifacts.get(&key) else {
                // SiteBuild validation makes this unreachable for plan roots and
                // artifact reads; retaining it keeps the proof defensive.
                blockers.push(SealBlocker::Missing { key });
                continue;
            };
            match &record.state {
                ArtifactState::Ready { .. } => {}
                ArtifactState::Deferred { reason } => blockers.push(SealBlocker::Deferred {
                    key: key.clone(),
                    reason: reason.clone(),
                }),
                ArtifactState::Unsupported { capability, reason } => {
                    blockers.push(SealBlocker::Unsupported {
                        key: key.clone(),
                        capability: capability.clone(),
                        reason: reason.clone(),
                    });
                }
                ArtifactState::Failed { diagnostics } => blockers.push(SealBlocker::Failed {
                    key: key.clone(),
                    diagnostics: diagnostics.clone(),
                }),
            }
            for dependency in &record.reads {
                if let ReadDependency::Artifact { key } = dependency {
                    pending.insert(key.clone());
                }
            }
        }

        if blockers.is_empty() {
            Ok(Self(build))
        } else {
            Err(SealError { blockers })
        }
    }
}

impl<'de> Deserialize<'de> for ClosedSiteBuild {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let build = SiteBuild::deserialize(deserializer)?;
        Self::try_from(build).map_err(de::Error::custom)
    }
}

fn validate_content(content: &ContentRef) -> Result<(), ContractError> {
    if content
        .media_type
        .as_deref()
        .is_some_and(|media_type| media_type.trim().is_empty())
    {
        return Err(ContractError::InvalidContentRef);
    }
    Ok(())
}

fn validate_artifact_key(key: &ArtifactKey) -> Result<(), ContractError> {
    let resource_valid = |resource: &ResourceKey| {
        !resource.resource_type.trim().is_empty() && !resource.id.trim().is_empty()
    };
    let valid = match key {
        ArtifactKey::SemanticModel { name } => !name.trim().is_empty(),
        ArtifactKey::Resource { resource } => resource_valid(resource),
        ArtifactKey::Fragment {
            scope,
            fragment,
            parameters,
        } => {
            let scope_valid = match scope {
                FragmentScope::WholeIg => true,
                FragmentScope::Resource { resource } => resource_valid(resource),
            };
            scope_valid
                && !parameters.keys().any(|key| key.trim().is_empty())
                && !matches!(fragment, FragmentKind::Other { name } if name.trim().is_empty())
        }
        ArtifactKey::Page { .. } => true,
        ArtifactKey::Asset { namespace, .. } => {
            !matches!(namespace, AssetNamespace::Other { name } if name.trim().is_empty())
        }
        ArtifactKey::Data { namespace, name } => {
            !namespace.trim().is_empty() && !name.trim().is_empty()
        }
    };
    if !valid {
        return Err(ContractError::InvalidArtifactKey(Box::new(key.clone())));
    }
    Ok(())
}

fn validate_diagnostic(diagnostic: &BuildDiagnostic) -> Result<(), ContractError> {
    if diagnostic.code.trim().is_empty()
        || diagnostic.message.trim().is_empty()
        || diagnostic
            .location
            .as_ref()
            .is_some_and(|location| location.line == 0)
    {
        return Err(ContractError::InvalidDiagnostic);
    }
    Ok(())
}

impl<'de> Deserialize<'de> for SiteBuild {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SiteBuildWire::deserialize(deserializer)?;
        let build = Self {
            schema_version: wire.schema_version,
            build_id: wire.build_id,
            project: wire.project,
            package_lock: wire.package_lock,
            render_target: wire.render_target,
            render_plan: wire.render_plan,
            artifacts: wire.artifacts,
            diagnostics: wire.diagnostics,
        };
        build.verify().map_err(de::Error::custom)?;
        Ok(build)
    }
}
