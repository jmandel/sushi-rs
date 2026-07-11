//! Renderer-neutral semantic result of FHIR guide preparation.
//!
//! This crate deliberately depends on neither `site_build` nor `site_db` so the
//! same prepared value can feed renderer manifests and optional relational
//! compatibility projections without reversing the dependency direction.

use std::collections::BTreeSet;
use std::fmt;

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

/// An authored asset and the exact project sources used to prepare it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedAsset {
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
    pub assets: Vec<PreparedAsset>,
}
