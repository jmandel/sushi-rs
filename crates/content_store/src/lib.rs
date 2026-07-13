//! Renderer- and package-neutral storage for immutable content-addressed bytes.
//!
//! [`ContentRef`] is the complete handoff value: SHA-256 and byte length are
//! intrinsic byte identity, while `media_type` is exact semantic metadata from
//! the producer. A store never guesses media types from byte signatures or
//! filenames. Reads return the same validated reference with bytes that have
//! been re-hashed and length-checked.

use std::fmt;
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use sha2::Digest;
use thiserror::Error;

/// A validated, lowercase SHA-256 digest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(type = "string"))]
#[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
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

/// Exact immutable-content reference shared by compilers, renderers, and hosts.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "wire-contract", ts(optional_fields))]
#[serde(rename_all = "camelCase")]
pub struct ContentRef {
    /// The algorithm is fixed to SHA-256 and encoded in the field name.
    #[cfg_attr(feature = "wire-contract", ts(type = "string"))]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
    pub sha256: Sha256Digest,
    #[cfg_attr(feature = "wire-contract", ts(type = "number"))]
    pub byte_length: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "wire-contract", schemars(with = "String"))]
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

    /// Verify intrinsic byte identity and exact producer-declared media metadata.
    pub fn verify(&self, bytes: &[u8]) -> Result<(), VerificationError> {
        if self
            .media_type
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(VerificationError::InvalidMediaType);
        }
        let actual_length = bytes.len() as u64;
        if actual_length != self.byte_length {
            return Err(VerificationError::Length {
                expected: self.byte_length,
                actual: actual_length,
            });
        }
        let actual = Sha256Digest::of_bytes(bytes);
        if actual != self.sha256 {
            return Err(VerificationError::Digest {
                expected: self.sha256.clone(),
                actual,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum VerificationError {
    #[error("content media type, when present, must be non-empty")]
    InvalidMediaType,
    #[error("content length mismatch: expected {expected}, got {actual}")]
    Length { expected: u64, actual: u64 },
    #[error("content digest mismatch: expected {expected}, got {actual}")]
    Digest {
        expected: Sha256Digest,
        actual: Sha256Digest,
    },
}

/// Bytes proven to match an exact content reference.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedContent {
    reference: ContentRef,
    bytes: Vec<u8>,
}

impl VerifiedContent {
    pub fn new(reference: ContentRef, bytes: Vec<u8>) -> Result<Self, VerificationError> {
        reference.verify(&bytes)?;
        Ok(Self { reference, bytes })
    }

    pub fn content_ref(&self) -> &ContentRef {
        &self.reference
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("content object {0} is absent")]
    Missing(Sha256Digest),
    #[error("content store root is not a real directory: {0}")]
    InvalidRoot(PathBuf),
    #[error("content object is not a regular file: {0}")]
    InvalidObject(PathBuf),
    #[error(transparent)]
    Verification(#[from] VerificationError),
    #[error("content store I/O at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Minimal contract for native, memory, and browser-host implementations.
/// Implementations verify on both publication and retrieval.
pub trait ContentStore {
    fn put(&self, content: &ContentRef, bytes: &[u8]) -> Result<(), StoreError>;
    fn read(&self, content: &ContentRef) -> Result<VerifiedContent, StoreError>;

    fn contains(&self, content: &ContentRef) -> Result<bool, StoreError> {
        match self.read(content) {
            Ok(_) => Ok(true),
            Err(StoreError::Missing(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// Filesystem CAS whose root directly contains lowercase SHA-256 object names.
#[derive(Clone, Debug)]
pub struct FileContentStore {
    root: PathBuf,
}

impl FileContentStore {
    pub fn create(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref();
        fs::create_dir_all(root).map_err(|source| StoreError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        Self::open(root)
    }

    /// Open an existing CAS root without manufacturing a missing authority
    /// directory. Readers should use this at trust boundaries; writers may use
    /// [`Self::create`] when they explicitly own publication.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let root = root.as_ref();
        let metadata = fs::symlink_metadata(root).map_err(|source| StoreError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(StoreError::InvalidRoot(root.to_path_buf()));
        }
        let root = root.canonicalize().map_err(|source| StoreError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn object_path(&self, digest: &Sha256Digest) -> PathBuf {
        self.root.join(digest.as_str())
    }

    fn read_bytes(&self, content: &ContentRef) -> Result<Vec<u8>, StoreError> {
        let path = self.object_path(&content.sha256);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::Missing(content.sha256.clone()));
            }
            Err(source) => return Err(StoreError::Io { path, source }),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(StoreError::InvalidObject(path));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .open(&path)
            .map_err(|source| StoreError::Io {
                path: path.clone(),
                source,
            })?;
        let opened = file.metadata().map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })?;
        let after = fs::symlink_metadata(&path).map_err(|source| StoreError::Io {
            path: path.clone(),
            source,
        })?;
        if after.file_type().is_symlink() || !opened.is_file() || !after.is_file() {
            return Err(StoreError::InvalidObject(path));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // Opening a symlink or swapping the directory entry between the
            // lstat/open/lstat sequence must not redirect a CAS read.
            if metadata.dev() != opened.dev()
                || metadata.ino() != opened.ino()
                || after.dev() != opened.dev()
                || after.ino() != opened.ino()
            {
                return Err(StoreError::InvalidObject(path));
            }
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len().min(content.byte_length)).unwrap_or(0),
        );
        file.read_to_end(&mut bytes)
            .map_err(|source| StoreError::Io { path, source })?;
        Ok(bytes)
    }

    fn sync_root(&self) -> Result<(), StoreError> {
        #[cfg(unix)]
        return File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| StoreError::Io {
                path: self.root.clone(),
                source,
            });
        #[cfg(not(unix))]
        Ok(())
    }
}

impl ContentStore for FileContentStore {
    fn put(&self, content: &ContentRef, bytes: &[u8]) -> Result<(), StoreError> {
        content.verify(bytes)?;
        let destination = self.object_path(&content.sha256);
        if fs::symlink_metadata(&destination).is_ok() {
            return self.read(content).map(|_| ());
        }

        let mut temporary =
            tempfile::NamedTempFile::new_in(&self.root).map_err(|source| StoreError::Io {
                path: self.root.clone(),
                source,
            })?;
        temporary
            .write_all(bytes)
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|source| StoreError::Io {
                path: temporary.path().to_path_buf(),
                source,
            })?;

        match temporary.persist_noclobber(&destination) {
            Ok(_) => self.sync_root(),
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {
                self.read(content).map(|_| ())
            }
            Err(error) => Err(StoreError::Io {
                path: destination,
                source: error.error,
            }),
        }
    }

    fn read(&self, content: &ContentRef) -> Result<VerifiedContent, StoreError> {
        VerifiedContent::new(content.clone(), self.read_bytes(content)?).map_err(Into::into)
    }
}
