use std::fmt;

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sha2::Digest;
use thiserror::Error;

/// A validated, lowercase SHA-256 digest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest(String);

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum DigestError {
    #[error("SHA-256 digest must contain exactly 64 lowercase hexadecimal characters")]
    InvalidSha256,
}

impl Sha256Digest {
    pub fn parse(value: impl Into<String>) -> Result<Self, DigestError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .as_bytes()
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(DigestError::InvalidSha256);
        }
        Ok(Self(value))
    }

    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(hex::encode(sha2::Sha256::digest(bytes)))
    }

    pub(crate) fn from_hash(hash: [u8; 32]) -> Self {
        Self(hex::encode(hash))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// Reference to immutable bytes in a content-addressed store.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub struct ContentRef {
    /// The algorithm is fixed to SHA-256 by the v1 schema and therefore encoded
    /// in the field name instead of repeated as an unconstrained string.
    pub sha256: Sha256Digest,
    pub byte_length: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

impl ContentRef {
    pub fn of_bytes(bytes: &[u8], media_type: Option<impl Into<String>>) -> Self {
        Self {
            sha256: Sha256Digest::of_bytes(bytes),
            byte_length: bytes.len() as u64,
            media_type: media_type.map(Into::into),
        }
    }
}

/// Deterministic identity of the complete SiteBuild document (except this field).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
