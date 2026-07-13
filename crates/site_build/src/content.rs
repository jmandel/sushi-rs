use std::fmt;

pub use content_store::{ContentRef, Sha256Digest};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// Deterministic identity of the complete SiteBuild document (except this field).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(type = "string"))]
#[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
pub struct BuildId(String);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum BuildIdError {
    #[error("build id must be 'sb1-sha256:' followed by 64 lowercase hexadecimal characters")]
    Invalid,
}

impl BuildId {
    const PREFIX: &'static str = "sb1-sha256:";

    pub(crate) fn from_digest(digest: Sha256Digest) -> Self {
        Self(format!("{}{digest}", Self::PREFIX))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, BuildIdError> {
        let value = value.into();
        let Some(digest) = value.strip_prefix(Self::PREFIX) else {
            return Err(BuildIdError::Invalid);
        };
        Sha256Digest::parse(digest).map_err(|_| BuildIdError::Invalid)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BuildId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for BuildId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BuildId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}
