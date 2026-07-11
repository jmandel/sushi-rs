//! Exact, renderer-neutral identity for one complete materialized site tree.
//!
//! [`OutputCacheKey`] is known before rendering and is therefore suitable for
//! cache lookup. It binds a closed SiteBuild to the renderer implementation and
//! recipe digest, output schema, and explicit options. [`SiteOutputId`] also
//! binds the sorted path/content inventory and is the post-render integrity
//! identity. Neither identity contains a host path or mutable project alias.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use content_store::{ContentStore, StoreError};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::{canonical_json_bytes, CanonicalError, ClosedSiteBuild, ContentRef, Sha256Digest};

pub const SITE_OUTPUT_SCHEMA: &str = "site-output/v1";
pub const SITE_OUTPUT_MANIFEST_PATH: &str = "site-output.json";

/// Exact closed SiteBuild input identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputInputId(String);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("output input id must be an sb1 SHA-256 identity")]
pub struct OutputInputIdError;

impl OutputInputId {
    pub fn from_closed(input: &ClosedSiteBuild) -> Self {
        Self(input.site_build().build_id().to_string())
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, OutputInputIdError> {
        let value = value.into();
        let digest = value
            .strip_prefix("sb1-sha256:")
            .ok_or(OutputInputIdError)?;
        Sha256Digest::parse(digest).map_err(|_| OutputInputIdError)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OutputInputId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for OutputInputId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OutputInputId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

/// Canonical normalized relative POSIX output path.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutputPath(String);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum OutputPathError {
    #[error("output path must be a normalized, non-empty relative POSIX path")]
    Invalid,
    #[error("output path collides with the reserved site output manifest")]
    Reserved,
}

impl OutputPath {
    pub fn parse(value: impl Into<String>) -> Result<Self, OutputPathError> {
        let value = value.into();
        let valid = !value.is_empty()
            && !value.starts_with('/')
            && !value.contains('\\')
            && !value.contains(':')
            && !value.chars().any(char::is_control)
            && value
                .split('/')
                .all(|component| !component.is_empty() && component != "." && component != "..");
        if !valid {
            return Err(OutputPathError::Invalid);
        }
        if value == SITE_OUTPUT_MANIFEST_PATH {
            return Err(OutputPathError::Reserved);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OutputPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for OutputPath {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for OutputPath {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RendererImplementation {
    pub id: String,
    pub version: String,
    /// Digest of the exact renderer code/assets/toolchain recipe used for this
    /// invocation. Human version labels alone are not cache-safe.
    pub recipe_sha256: Sha256Digest,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputProducer {
    pub id: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SiteOutputFile {
    pub path: OutputPath,
    pub content: ContentRef,
    pub producer: OutputProducer,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<OutputPath>,
}

macro_rules! prefixed_id {
    ($name:ident, $error:ident, $prefix:literal, $message:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        #[derive(Clone, Debug, Error, PartialEq, Eq)]
        #[error($message)]
        pub struct $error;

        impl $name {
            fn from_digest(digest: Sha256Digest) -> Self {
                Self(format!("{}{}", $prefix, digest))
            }

            pub fn parse(value: impl Into<String>) -> Result<Self, $error> {
                let value = value.into();
                let Some(digest) = value.strip_prefix($prefix) else {
                    return Err($error);
                };
                Sha256Digest::parse(digest).map_err(|_| $error)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::parse(String::deserialize(deserializer)?).map_err(de::Error::custom)
            }
        }
    };
}

prefixed_id!(
    OutputCacheKey,
    OutputCacheKeyError,
    "sok1-sha256:",
    "output cache key must be 'sok1-sha256:' followed by 64 lowercase hexadecimal characters"
);
prefixed_id!(
    SiteOutputId,
    SiteOutputIdError,
    "so1-sha256:",
    "site output id must be 'so1-sha256:' followed by 64 lowercase hexadecimal characters"
);

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SiteOutput {
    schema_version: String,
    input_build_id: OutputInputId,
    renderer: RendererImplementation,
    output_schema: String,
    options: BTreeMap<String, String>,
    cache_key: OutputCacheKey,
    files: Vec<SiteOutputFile>,
    output_id: SiteOutputId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SiteOutputWire {
    schema_version: String,
    input_build_id: OutputInputId,
    renderer: RendererImplementation,
    output_schema: String,
    options: BTreeMap<String, String>,
    cache_key: OutputCacheKey,
    files: Vec<SiteOutputFile>,
    output_id: SiteOutputId,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CacheKeyPayload<'a> {
    schema_version: &'a str,
    input_build_id: &'a OutputInputId,
    renderer: &'a RendererImplementation,
    output_schema: &'a str,
    options: &'a BTreeMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputIdPayload<'a> {
    schema_version: &'a str,
    input_build_id: &'a OutputInputId,
    renderer: &'a RendererImplementation,
    output_schema: &'a str,
    options: &'a BTreeMap<String, String>,
    cache_key: &'a OutputCacheKey,
    files: &'a [SiteOutputFile],
}

#[derive(Debug, Error)]
pub enum SiteOutputError {
    #[error("unsupported site output schema {0}")]
    UnsupportedSchema(String),
    #[error("renderer id/version, output schema, option keys, producer id/version, and optional source must be non-empty trimmed strings without NUL")]
    EmptyIdentity,
    #[error("duplicate output path {0}")]
    DuplicatePath(OutputPath),
    #[error("output files are not in canonical path order")]
    NonCanonicalOrder,
    #[error("output {path} names missing owner {owner}")]
    MissingOwner { path: OutputPath, owner: OutputPath },
    #[error("invalid content reference for output {0}")]
    InvalidContent(OutputPath),
    #[error(
        "output cache key mismatch: document has {actual}, canonical inputs require {expected}"
    )]
    CacheKeyMismatch {
        actual: OutputCacheKey,
        expected: OutputCacheKey,
    },
    #[error(
        "site output id mismatch: document has {actual}, canonical content requires {expected}"
    )]
    OutputIdMismatch {
        actual: SiteOutputId,
        expected: SiteOutputId,
    },
    #[error("site output was produced for {actual}, not requested closed build {expected}")]
    InputBuildMismatch {
        actual: OutputInputId,
        expected: OutputInputId,
    },
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl SiteOutput {
    pub fn new(
        input: &ClosedSiteBuild,
        renderer: RendererImplementation,
        output_schema: impl Into<String>,
        options: BTreeMap<String, String>,
        files: impl IntoIterator<Item = SiteOutputFile>,
    ) -> Result<Self, SiteOutputError> {
        let mut files: Vec<_> = files.into_iter().collect();
        files.sort_by(|left, right| left.path.cmp(&right.path));
        let placeholder = Sha256Digest::of_bytes(&[]);
        let mut output = Self {
            schema_version: SITE_OUTPUT_SCHEMA.into(),
            input_build_id: OutputInputId::from_closed(input),
            renderer,
            output_schema: output_schema.into(),
            options,
            cache_key: OutputCacheKey::from_digest(placeholder.clone()),
            files,
            output_id: SiteOutputId::from_digest(placeholder),
        };
        output.validate_contract()?;
        output.cache_key = output.expected_cache_key()?;
        output.output_id = output.expected_output_id()?;
        Ok(output)
    }

    pub fn schema_version(&self) -> &str {
        &self.schema_version
    }
    pub fn input_build_id(&self) -> &OutputInputId {
        &self.input_build_id
    }
    pub fn renderer(&self) -> &RendererImplementation {
        &self.renderer
    }
    pub fn output_schema(&self) -> &str {
        &self.output_schema
    }
    pub fn options(&self) -> &BTreeMap<String, String> {
        &self.options
    }
    pub fn cache_key(&self) -> &OutputCacheKey {
        &self.cache_key
    }
    pub fn files(&self) -> &[SiteOutputFile] {
        &self.files
    }
    pub fn output_id(&self) -> &SiteOutputId {
        &self.output_id
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_json_bytes(self)
    }

    pub fn verify(&self) -> Result<(), SiteOutputError> {
        if self.schema_version != SITE_OUTPUT_SCHEMA {
            return Err(SiteOutputError::UnsupportedSchema(
                self.schema_version.clone(),
            ));
        }
        self.validate_contract()?;
        let expected_cache_key = self.expected_cache_key()?;
        if self.cache_key != expected_cache_key {
            return Err(SiteOutputError::CacheKeyMismatch {
                actual: self.cache_key.clone(),
                expected: expected_cache_key,
            });
        }
        let expected_output_id = self.expected_output_id()?;
        if self.output_id != expected_output_id {
            return Err(SiteOutputError::OutputIdMismatch {
                actual: self.output_id.clone(),
                expected: expected_output_id,
            });
        }
        Ok(())
    }

    pub fn verify_for(&self, input: &ClosedSiteBuild) -> Result<(), SiteOutputError> {
        self.verify()?;
        let expected = OutputInputId::from_closed(input);
        if self.input_build_id != expected {
            return Err(SiteOutputError::InputBuildMismatch {
                actual: self.input_build_id.clone(),
                expected,
            });
        }
        Ok(())
    }

    pub fn verify_store(&self, store: &dyn ContentStore) -> Result<(), SiteOutputError> {
        self.verify()?;
        for file in &self.files {
            store.read(&file.content)?;
        }
        Ok(())
    }

    /// Complete cache-hit gate: exact closed input plus every addressed byte.
    pub fn verify_cached(
        &self,
        input: &ClosedSiteBuild,
        store: &dyn ContentStore,
    ) -> Result<(), SiteOutputError> {
        self.verify_for(input)?;
        for file in &self.files {
            store.read(&file.content)?;
        }
        Ok(())
    }

    fn expected_cache_key(&self) -> Result<OutputCacheKey, CanonicalError> {
        let bytes = canonical_json_bytes(&CacheKeyPayload {
            schema_version: &self.schema_version,
            input_build_id: &self.input_build_id,
            renderer: &self.renderer,
            output_schema: &self.output_schema,
            options: &self.options,
        })?;
        Ok(OutputCacheKey::from_digest(Sha256Digest::of_bytes(&bytes)))
    }

    fn expected_output_id(&self) -> Result<SiteOutputId, CanonicalError> {
        let bytes = canonical_json_bytes(&OutputIdPayload {
            schema_version: &self.schema_version,
            input_build_id: &self.input_build_id,
            renderer: &self.renderer,
            output_schema: &self.output_schema,
            options: &self.options,
            cache_key: &self.cache_key,
            files: &self.files,
        })?;
        Ok(SiteOutputId::from_digest(Sha256Digest::of_bytes(&bytes)))
    }

    fn validate_contract(&self) -> Result<(), SiteOutputError> {
        fn valid(value: &str) -> bool {
            !value.is_empty() && value == value.trim() && !value.contains('\0')
        }
        if !valid(&self.renderer.id)
            || !valid(&self.renderer.version)
            || !valid(&self.output_schema)
            || self.options.keys().any(|key| !valid(key))
        {
            return Err(SiteOutputError::EmptyIdentity);
        }
        let mut paths = BTreeSet::new();
        let mut previous = None;
        for file in &self.files {
            if previous.as_ref().is_some_and(|path| path >= &file.path) {
                if previous.as_ref() == Some(&file.path) {
                    return Err(SiteOutputError::DuplicatePath(file.path.clone()));
                }
                return Err(SiteOutputError::NonCanonicalOrder);
            }
            previous = Some(file.path.clone());
            if !paths.insert(file.path.clone()) {
                return Err(SiteOutputError::DuplicatePath(file.path.clone()));
            }
            if !valid(&file.producer.id)
                || !valid(&file.producer.version)
                || file.source.as_deref().is_some_and(|value| !valid(value))
            {
                return Err(SiteOutputError::EmptyIdentity);
            }
            if file
                .content
                .media_type
                .as_deref()
                .is_none_or(|media_type| !valid(media_type))
            {
                return Err(SiteOutputError::InvalidContent(file.path.clone()));
            }
        }
        for file in &self.files {
            if let Some(owner) = &file.owner {
                if !paths.contains(owner) {
                    return Err(SiteOutputError::MissingOwner {
                        path: file.path.clone(),
                        owner: owner.clone(),
                    });
                }
            }
        }
        Ok(())
    }
}

impl OutputCacheKey {
    /// Derive the lookup key before rendering any output bytes.
    pub fn for_closed(
        input: &ClosedSiteBuild,
        renderer: &RendererImplementation,
        output_schema: &str,
        options: &BTreeMap<String, String>,
    ) -> Result<Self, SiteOutputError> {
        fn valid(value: &str) -> bool {
            !value.is_empty() && value == value.trim() && !value.contains('\0')
        }
        if !valid(&renderer.id)
            || !valid(&renderer.version)
            || !valid(output_schema)
            || options.keys().any(|key| !valid(key))
        {
            return Err(SiteOutputError::EmptyIdentity);
        }
        let input_build_id = OutputInputId::from_closed(input);
        let bytes = canonical_json_bytes(&CacheKeyPayload {
            schema_version: SITE_OUTPUT_SCHEMA,
            input_build_id: &input_build_id,
            renderer,
            output_schema,
            options,
        })?;
        Ok(Self::from_digest(Sha256Digest::of_bytes(&bytes)))
    }
}

impl<'de> Deserialize<'de> for SiteOutput {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SiteOutputWire::deserialize(deserializer)?;
        let output = Self {
            schema_version: wire.schema_version,
            input_build_id: wire.input_build_id,
            renderer: wire.renderer,
            output_schema: wire.output_schema,
            options: wire.options,
            cache_key: wire.cache_key,
            files: wire.files,
            output_id: wire.output_id,
        };
        output.verify().map_err(de::Error::custom)?;
        Ok(output)
    }
}
