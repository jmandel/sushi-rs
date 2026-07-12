//! Versioned, compact package artifacts for deterministic warm mounting.
//!
//! PreparedPackage v3 keeps a canonical metadata directory in front of
//! independently compressed chunks. Mounting authenticates and validates that
//! directory without inflating package bodies. A body is exposed only after its
//! chunk and member digests have been checked; verified chunks are retained in a
//! bounded cache shared by every package using the same backing object.
//! The SHA-256 of the exact complete carrier is computed once at encode/decode
//! and retained for its enclosing ContentRef; there is no redundant checksum
//! footer inside the content-addressed artifact.

use crate::material::{
    canonicalize_package_material, finish_normalized_package_material, parse_exact_package_label,
    validate_member_name, CanonicalPackageMaterial,
};
use crate::{derived_index, BundleSource};
use anyhow::{anyhow, bail, Context, Result};
use miniz_oxide::{deflate, inflate};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::OnceCell;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::ops::Range;
use std::rc::Rc;
use std::str::FromStr;

const MAGIC: &[u8; 8] = b"FHIRPPK\0";
const SOURCE_ID_DOMAIN: &[u8] = b"fhir-prepared-package-source-v3\0";
const CHUNK_TARGET_BYTES: usize = 1024 * 1024;
// Level 1 retains a strong JSON compression ratio while keeping first-use
// browser preparation bounded. Level 6 was ~1.9x slower on R4 core for a warm
// artifact only ~40% smaller; both remain far below the expanded v1 image.
const COMPRESSION_LEVEL: u8 = 1;
// Current browser staging creates one backing per artifact. Keep the per-backing
// budget low enough that a 17-package closure cannot retain more than 136 MiB.
// A future host-wide/shared LRU can use a larger aggregate budget.
const DECOMPRESSED_CACHE_BYTES: usize = 8 * 1024 * 1024;

/// Binary container version. Bump if the layout, identity construction,
/// compression recipe, or deterministic packing changes.
pub const PREPARED_PACKAGE_FORMAT_VERSION: u32 = 3;
/// Package normalization algorithm version. Bump when canonical mounted bytes
/// or semantic payload selection changes.
pub const PACKAGE_NORMALIZATION_VERSION: u32 = 1;
/// ABI of the package read/mount interpretation.
pub const PACKAGE_ENGINE_ABI_VERSION: u32 = 1;
/// Stable media type for the compact prepared package artifact.
pub const PREPARED_PACKAGE_MEDIA_TYPE: &str = "application/vnd.fhir.package.prepared.v3";

/// Complete cache identity for a prepared package.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedPackageKey {
    pub source_sha256: String,
    pub format_version: u32,
    pub normalization_version: u32,
    pub derived_index_version: u32,
    pub engine_abi_version: u32,
}

impl PreparedPackageKey {
    fn current(source_digest: [u8; 32]) -> Self {
        Self {
            source_sha256: hex::encode(source_digest),
            format_version: PREPARED_PACKAGE_FORMAT_VERSION,
            normalization_version: PACKAGE_NORMALIZATION_VERSION,
            derived_index_version: derived_index::DERIVED_INDEX_FORMAT_VERSION,
            engine_abi_version: PACKAGE_ENGINE_ABI_VERSION,
        }
    }

    pub fn cache_key(&self) -> String {
        format!(
            "pp{}-sha256-{}-n{}-d{}-a{}",
            self.format_version,
            self.source_sha256,
            self.normalization_version,
            self.derived_index_version,
            self.engine_abi_version
        )
    }

    fn source_digest(&self) -> Result<[u8; 32]> {
        parse_canonical_digest(&self.source_sha256, "prepared-package key sourceSha256")
    }

    fn require_current(&self) -> Result<()> {
        if self.format_version != PREPARED_PACKAGE_FORMAT_VERSION
            || self.normalization_version != PACKAGE_NORMALIZATION_VERSION
            || self.derived_index_version != derived_index::DERIVED_INDEX_FORMAT_VERSION
            || self.engine_abi_version != PACKAGE_ENGINE_ABI_VERSION
        {
            bail!(
                "prepared-package key format tuple is unsupported: format={}, normalization={}, derived-index={}, engine-abi={}",
                self.format_version,
                self.normalization_version,
                self.derived_index_version,
                self.engine_abi_version
            );
        }
        self.source_digest()?;
        Ok(())
    }
}

fn parse_canonical_digest(value: &str, field: &str) -> Result<[u8; 32]> {
    let bytes = hex::decode(value).with_context(|| format!("{field} is not hex"))?;
    let digest: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow!("{field} must contain 32 bytes"))?;
    if hex::encode(digest) != value {
        bail!("{field} must be canonical lowercase hex");
    }
    Ok(digest)
}

impl fmt::Display for PreparedPackageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.cache_key())
    }
}

impl FromStr for PreparedPackageKey {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let (format, rest) = value
            .strip_prefix("pp")
            .and_then(|v| v.split_once("-sha256-"))
            .ok_or_else(|| anyhow!("invalid prepared-package cache key"))?;
        let (digest, rest) = rest
            .split_once("-n")
            .ok_or_else(|| anyhow!("invalid prepared-package cache key"))?;
        let (normalization, rest) = rest
            .split_once("-d")
            .ok_or_else(|| anyhow!("invalid prepared-package cache key"))?;
        let (derived_index, engine_abi) = rest
            .split_once("-a")
            .ok_or_else(|| anyhow!("invalid prepared-package cache key"))?;
        let key = Self {
            source_sha256: digest.to_string(),
            format_version: format.parse().context("invalid prepared-package format")?,
            normalization_version: normalization
                .parse()
                .context("invalid prepared-package normalization version")?,
            derived_index_version: derived_index
                .parse()
                .context("invalid prepared-package derived-index version")?,
            engine_abi_version: engine_abi
                .parse()
                .context("invalid prepared-package engine ABI")?,
        };
        key.require_current()?;
        if key.cache_key() != value {
            bail!("prepared-package cache key is not canonical");
        }
        Ok(key)
    }
}

#[derive(Clone, Debug)]
pub struct PreparedPackage {
    pub label: String,
    pub key: PreparedPackageKey,
    pub files: PreparedFiles,
    pub declared_dependencies: BTreeMap<String, String>,
    artifact: OnceCell<ExactArtifact>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ExactArtifact {
    bytes: Rc<Vec<u8>>,
    sha256: String,
}

impl PartialEq for PreparedPackage {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label
            && self.key == other.key
            && self.files == other.files
            && self.declared_dependencies == other.declared_dependencies
    }
}

impl Eq for PreparedPackage {}

#[derive(Clone, Debug)]
pub struct PreparedFiles(PreparedFileStorage);

#[derive(Clone, Debug)]
enum PreparedFileStorage {
    Owned(BTreeMap<String, Vec<u8>>),
    Compressed(PreparedCompressedFiles),
}

/// A reusable backing for a whole contiguous prepared-package batch. Hosts that
/// decode several ranges should create one and call `decode_backing_range`, so
/// packages share both the compressed allocation and the bounded chunk cache.
#[derive(Clone, Debug)]
pub struct PreparedArtifactBacking(Rc<CompressedBacking>);

impl PreparedArtifactBacking {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self::from_shared(Rc::new(bytes))
    }

    pub fn from_shared(bytes: Rc<Vec<u8>>) -> Self {
        Self(Rc::new(CompressedBacking {
            bytes,
            cache: RefCell::new(ChunkCache::default()),
        }))
    }

    pub fn len(&self) -> usize {
        self.0.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.bytes.is_empty()
    }
}

#[derive(Debug)]
struct CompressedBacking {
    bytes: Rc<Vec<u8>>,
    cache: RefCell<ChunkCache>,
}

#[derive(Debug, Default)]
struct ChunkCache {
    entries: BTreeMap<usize, CachedChunk>,
    retained_bytes: usize,
    tick: u64,
    cache_hits: u64,
    chunks_inflated: u64,
    raw_inflated_bytes: u64,
}

#[derive(Debug)]
struct CachedChunk {
    result: std::result::Result<Rc<Vec<u8>>, String>,
    bytes: usize,
    last_used: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompressedChunk {
    compressed: Range<usize>,
    raw_len: usize,
    raw_sha256: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompressedMember {
    chunk: Rc<CompressedChunk>,
    raw: Range<usize>,
    raw_sha256: [u8; 32],
}

#[derive(Clone, Debug)]
pub(crate) struct PreparedCompressedFiles {
    backing: PreparedArtifactBacking,
    members: BTreeMap<String, CompressedMember>,
    artifact_range: Range<usize>,
}

impl CompressedBacking {
    fn read_chunk(&self, chunk: &CompressedChunk) -> io::Result<Rc<Vec<u8>>> {
        let key = chunk.compressed.start;
        {
            let mut cache = self.cache.borrow_mut();
            cache.tick = cache.tick.wrapping_add(1);
            let tick = cache.tick;
            if let Some(entry) = cache.entries.get_mut(&key) {
                entry.last_used = tick;
                let result = entry.result.clone();
                cache.cache_hits = cache.cache_hits.saturating_add(1);
                return result.map_err(invalid_data);
            }
        }

        let result = (|| {
            let compressed = self
                .bytes
                .get(chunk.compressed.clone())
                .ok_or_else(|| "compressed chunk range is outside its backing".to_string())?;
            let raw = inflate::decompress_to_vec_with_limit(compressed, chunk.raw_len)
                .map_err(|error| format!("raw-DEFLATE chunk failed to inflate: {error:?}"))?;
            if raw.len() != chunk.raw_len {
                return Err(format!(
                    "inflated chunk length mismatch: expected {}, received {}",
                    chunk.raw_len,
                    raw.len()
                ));
            }
            let actual: [u8; 32] = Sha256::digest(&raw).into();
            if actual != chunk.raw_sha256 {
                return Err("inflated chunk SHA-256 mismatch".into());
            }
            Ok(Rc::new(raw))
        })();

        let mut cache = self.cache.borrow_mut();
        cache.tick = cache.tick.wrapping_add(1);
        let tick = cache.tick;
        let bytes = result.as_ref().map_or(0, |value| value.len());
        if result.is_ok() {
            cache.chunks_inflated = cache.chunks_inflated.saturating_add(1);
            cache.raw_inflated_bytes = cache.raw_inflated_bytes.saturating_add(bytes as u64);
        }
        if bytes <= DECOMPRESSED_CACHE_BYTES {
            while cache.retained_bytes.saturating_add(bytes) > DECOMPRESSED_CACHE_BYTES {
                let Some((&oldest, _)) = cache.entries.iter().min_by_key(|(_, v)| v.last_used)
                else {
                    break;
                };
                if let Some(removed) = cache.entries.remove(&oldest) {
                    cache.retained_bytes = cache.retained_bytes.saturating_sub(removed.bytes);
                }
            }
            cache.retained_bytes = cache.retained_bytes.saturating_add(bytes);
            cache.entries.insert(
                key,
                CachedChunk {
                    result: result.clone(),
                    bytes,
                    last_used: tick,
                },
            );
        }
        result.map_err(invalid_data)
    }
}

fn invalid_data(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

impl PreparedCompressedFiles {
    pub(crate) fn names(&self) -> impl Iterator<Item = &String> {
        self.members.keys()
    }

    pub(crate) fn contains_key(&self, name: &str) -> bool {
        self.members.contains_key(name)
    }

    pub(crate) fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        let member = self
            .members
            .get(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such prepared member"))?;
        let chunk = self.backing.0.read_chunk(&member.chunk)?;
        let body = chunk
            .get(member.raw.clone())
            .ok_or_else(|| invalid_data("member range is outside inflated chunk".into()))?;
        let actual: [u8; 32] = Sha256::digest(body).into();
        if actual != member.raw_sha256 {
            return Err(invalid_data("prepared member SHA-256 mismatch".into()));
        }
        Ok(body.to_vec())
    }

    pub(crate) fn backing_identity(&self) -> usize {
        Rc::as_ptr(&self.backing.0) as usize
    }

    pub(crate) fn backing_metrics(&self) -> PreparedBackingMetrics {
        let cache = self.backing.0.cache.borrow();
        PreparedBackingMetrics {
            compressed_retained_bytes: self.backing.0.bytes.len() as u64,
            chunks_inflated: cache.chunks_inflated,
            raw_inflated_bytes: cache.raw_inflated_bytes,
            cache_hits: cache.cache_hits,
            cached_raw_bytes: cache.retained_bytes as u64,
        }
    }

    pub(crate) fn declared_raw_bytes(&self) -> u64 {
        let mut chunks = BTreeMap::new();
        for member in self.members.values() {
            chunks
                .entry(member.chunk.compressed.start)
                .or_insert(member.chunk.raw_len as u64);
        }
        chunks.values().copied().sum()
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct PreparedBackingMetrics {
    pub compressed_retained_bytes: u64,
    pub chunks_inflated: u64,
    pub raw_inflated_bytes: u64,
    pub cache_hits: u64,
    pub cached_raw_bytes: u64,
}

impl PreparedFiles {
    pub fn contains_key(&self, name: &str) -> bool {
        match &self.0 {
            PreparedFileStorage::Owned(files) => files.contains_key(name),
            PreparedFileStorage::Compressed(files) => files.contains_key(name),
        }
    }

    pub fn read(&self, name: &str) -> io::Result<Option<Vec<u8>>> {
        match &self.0 {
            PreparedFileStorage::Owned(files) => Ok(files.get(name).cloned()),
            PreparedFileStorage::Compressed(files) => {
                if files.contains_key(name) {
                    files.read(name).map(Some)
                } else {
                    Ok(None)
                }
            }
        }
    }

    /// Materialize the complete validated file map. Native package-preparation
    /// tools use this only while folding sibling template files into a new
    /// canonical carrier; browser execution remains member-lazy.
    pub fn materialize_all(&self) -> io::Result<BTreeMap<String, Vec<u8>>> {
        let mut files = BTreeMap::new();
        for member in self.metadata() {
            let bytes = self.read(&member.name)?.ok_or_else(|| {
                invalid_data(format!("prepared member {} disappeared", member.name))
            })?;
            files.insert(member.name, bytes);
        }
        Ok(files)
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            PreparedFileStorage::Owned(files) => files.len(),
            PreparedFileStorage::Compressed(files) => files.members.len(),
        }
    }

    /// Total canonical uncompressed member bytes, available from compact
    /// metadata without inflating any chunk.
    pub fn raw_bytes(&self) -> u64 {
        self.metadata().iter().map(|member| member.raw_len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_blob_backed(&self) -> bool {
        matches!(self.0, PreparedFileStorage::Compressed(_))
    }

    pub fn shares_backing_blob_with(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (PreparedFileStorage::Compressed(left), PreparedFileStorage::Compressed(right)) => {
                Rc::ptr_eq(&left.backing.0.bytes, &right.backing.0.bytes)
            }
            _ => false,
        }
    }

    fn original_artifact(&self) -> Option<Rc<Vec<u8>>> {
        let PreparedFileStorage::Compressed(files) = &self.0 else {
            return None;
        };
        if files.artifact_range.start == 0
            && files.artifact_range.end == files.backing.0.bytes.len()
        {
            return Some(files.backing.0.bytes.clone());
        }
        files
            .backing
            .0
            .bytes
            .get(files.artifact_range.clone())
            .map(|bytes| Rc::new(bytes.to_vec()))
    }

    fn mount_into(self, source: &mut BundleSource, label: &str) {
        match self.0 {
            PreparedFileStorage::Owned(files) => source.mount_package(label, files),
            PreparedFileStorage::Compressed(files) => {
                source.mount_prepared_compressed(label, files)
            }
        }
    }

    fn metadata(&self) -> Vec<MemberIdentity> {
        match &self.0 {
            PreparedFileStorage::Owned(files) => files
                .iter()
                .map(|(name, body)| MemberIdentity {
                    name: name.clone(),
                    raw_len: body.len() as u64,
                    raw_sha256: Sha256::digest(body).into(),
                })
                .collect(),
            PreparedFileStorage::Compressed(files) => files
                .members
                .iter()
                .map(|(name, member)| MemberIdentity {
                    name: name.clone(),
                    raw_len: member.raw.len() as u64,
                    raw_sha256: member.raw_sha256,
                })
                .collect(),
        }
    }
}

impl PartialEq for PreparedFiles {
    fn eq(&self, other: &Self) -> bool {
        self.metadata() == other.metadata()
    }
}
impl Eq for PreparedFiles {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedPackageMount {
    pub label: String,
    pub key: PreparedPackageKey,
    pub declared_dependencies: BTreeMap<String, String>,
    artifact: ExactArtifact,
}

impl PreparedPackageMount {
    /// Exact deterministic prepared-package carrier bytes. Reading this value
    /// never inflates a member body. The returned allocation is shared by every
    /// caller holding this mount.
    pub fn artifact_bytes(&self) -> Rc<Vec<u8>> {
        self.artifact.bytes.clone()
    }

    /// Canonical SHA-256 of the exact complete carrier bytes. The digest was
    /// computed once while the artifact was encoded or shallow-decoded.
    pub fn artifact_sha256(&self) -> &str {
        &self.artifact.sha256
    }
}

pub struct PreparedPackageBuilder {
    label: String,
    material: CanonicalPackageMaterial,
}

impl PreparedPackageBuilder {
    pub fn build(self) -> Result<PreparedPackage> {
        let material = finish_normalized_package_material(&self.label, self.material)?;
        let members = identities_from_files(&material.files);
        let source_digest = source_metadata_digest(
            &self.label,
            &material.declared_dependencies,
            members
                .iter()
                .filter(|member| member.name != derived_index::SIDECAR_NAME),
        );
        Ok(PreparedPackage {
            label: self.label,
            key: PreparedPackageKey::current(source_digest),
            files: PreparedFiles(PreparedFileStorage::Owned(material.files)),
            declared_dependencies: material.declared_dependencies,
            artifact: OnceCell::new(),
        })
    }
}

impl PreparedPackage {
    pub fn prepare(label: &str, entries: BTreeMap<String, Vec<u8>>) -> Result<Self> {
        Self::normalize(label, entries)?.build()
    }

    pub fn normalize(
        label: &str,
        entries: BTreeMap<String, Vec<u8>>,
    ) -> Result<PreparedPackageBuilder> {
        Ok(PreparedPackageBuilder {
            label: label.to_string(),
            material: canonicalize_package_material(label, entries)?,
        })
    }

    /// Encode deterministic v3 bytes. Decoded packages retain their original
    /// exact artifact, so decode/encode is also byte preserving.
    pub fn encode(&self) -> Vec<u8> {
        self.artifact_bytes().as_ref().clone()
    }

    /// Exact deterministic prepared-package carrier bytes. The first call for
    /// an owned package encodes once; decoded packages copy only their exact
    /// artifact range. Subsequent calls and the mounted value share one `Rc`.
    pub fn artifact_bytes(&self) -> Rc<Vec<u8>> {
        self.exact_artifact().bytes.clone()
    }

    /// Canonical SHA-256 of the exact complete carrier bytes. This never
    /// re-hashes the artifact.
    pub fn artifact_sha256(&self) -> &str {
        &self.exact_artifact().sha256
    }

    fn exact_artifact(&self) -> &ExactArtifact {
        self.artifact.get_or_init(|| {
            let bytes = match self.files.original_artifact() {
                Some(bytes) => bytes,
                None => {
                    let PreparedFileStorage::Owned(files) = &self.files.0 else {
                        unreachable!()
                    };
                    Rc::new(encode_owned(self, files).expect("a prepared package always encodes"))
                }
            };
            let sha256 = hex::encode(Sha256::digest(bytes.as_ref()));
            ExactArtifact { bytes, sha256 }
        })
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let backing = PreparedArtifactBacking::new(bytes.to_vec());
        decode_shared(backing, 0..bytes.len(), None)
    }

    pub fn decode_expected(bytes: &[u8], expected: &PreparedPackageKey) -> Result<Self> {
        expected.require_current()?;
        let backing = PreparedArtifactBacking::new(bytes.to_vec());
        decode_shared(backing, 0..bytes.len(), Some(expected))
    }

    pub fn decode_owned(bytes: Vec<u8>, expected: &PreparedPackageKey) -> Result<Self> {
        expected.require_current()?;
        let length = bytes.len();
        decode_shared(
            PreparedArtifactBacking::new(bytes),
            0..length,
            Some(expected),
        )
    }

    /// Compatibility entry point. Prefer `decode_backing_range` for a batch so
    /// all decoded packages share one decompressed-chunk cache as well as bytes.
    pub fn decode_batch_range(
        blob: Rc<Vec<u8>>,
        range: Range<usize>,
        expected: &PreparedPackageKey,
    ) -> Result<Self> {
        Self::decode_backing_range(PreparedArtifactBacking::from_shared(blob), range, expected)
    }

    pub fn decode_backing_range(
        backing: PreparedArtifactBacking,
        range: Range<usize>,
        expected: &PreparedPackageKey,
    ) -> Result<Self> {
        expected.require_current()?;
        decode_shared(backing, range, Some(expected))
    }

    pub fn mount_into(self, source: &mut BundleSource) -> PreparedPackageMount {
        let artifact = self.exact_artifact().clone();
        let Self {
            label,
            key,
            files,
            declared_dependencies,
            artifact: _,
        } = self;
        files.mount_into(source, &label);
        PreparedPackageMount {
            label,
            key,
            declared_dependencies,
            artifact,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MemberIdentity {
    name: String,
    raw_len: u64,
    raw_sha256: [u8; 32],
}

#[derive(Clone, Debug)]
struct EncodedMember {
    identity: MemberIdentity,
    chunk: u32,
    raw_offset: u64,
}

struct EncodedChunk {
    raw_len: u64,
    raw_sha256: [u8; 32],
    compressed: Vec<u8>,
}

fn encode_owned(package: &PreparedPackage, files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let identities = identities_from_files(files);
    let source = source_metadata_digest(
        &package.label,
        &package.declared_dependencies,
        identities
            .iter()
            .filter(|member| member.name != derived_index::SIDECAR_NAME),
    );
    if source != package.key.source_digest()? {
        bail!("prepared-package source metadata disagrees with its key");
    }
    let mut encoded_members: Vec<EncodedMember> = identities
        .iter()
        .cloned()
        .map(|identity| EncodedMember {
            identity,
            chunk: u32::MAX,
            raw_offset: 0,
        })
        .collect();
    let by_name: BTreeMap<_, _> = encoded_members
        .iter()
        .enumerate()
        .map(|(index, member)| (member.identity.name.clone(), index))
        .collect();
    let mut chunks = Vec::new();

    // These two members are read during package setup and get independent
    // chunks, preventing either read from expanding unrelated resources.
    for hot in ["package.json", derived_index::SIDECAR_NAME] {
        if let Some(&index) = by_name.get(hot) {
            append_chunk(&mut encoded_members, files, &[index], &mut chunks)?;
        }
    }

    let mut pending = Vec::new();
    let mut pending_bytes = 0usize;
    for index in 0..encoded_members.len() {
        let name = &encoded_members[index].identity.name;
        if name == "package.json" || name == derived_index::SIDECAR_NAME {
            continue;
        }
        let length = files[name].len();
        if !pending.is_empty() && pending_bytes.saturating_add(length) > CHUNK_TARGET_BYTES {
            append_chunk(&mut encoded_members, files, &pending, &mut chunks)?;
            pending.clear();
            pending_bytes = 0;
        }
        pending.push(index);
        pending_bytes = pending_bytes.saturating_add(length);
        if length >= CHUNK_TARGET_BYTES {
            append_chunk(&mut encoded_members, files, &pending, &mut chunks)?;
            pending.clear();
            pending_bytes = 0;
        }
    }
    if !pending.is_empty() {
        append_chunk(&mut encoded_members, files, &pending, &mut chunks)?;
    }

    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    put_u32(&mut out, package.key.format_version);
    put_u32(&mut out, package.key.normalization_version);
    put_u32(&mut out, package.key.derived_index_version);
    put_u32(&mut out, package.key.engine_abi_version);
    out.extend_from_slice(&source);
    put_string(&mut out, &package.label)?;
    put_u32(
        &mut out,
        u32_len(package.declared_dependencies.len(), "dependency count")?,
    );
    for (name, version) in &package.declared_dependencies {
        put_string(&mut out, name)?;
        put_string(&mut out, version)?;
    }
    put_u32(&mut out, u32_len(encoded_members.len(), "member count")?);
    put_u32(&mut out, u32_len(chunks.len(), "chunk count")?);
    for member in &encoded_members {
        put_string(&mut out, &member.identity.name)?;
        put_u32(&mut out, member.chunk);
        put_u64(&mut out, member.raw_offset);
        put_u64(&mut out, member.identity.raw_len);
        out.extend_from_slice(&member.identity.raw_sha256);
    }
    for chunk in &chunks {
        put_u64(&mut out, chunk.raw_len);
        put_u64(&mut out, chunk.compressed.len() as u64);
        out.extend_from_slice(&chunk.raw_sha256);
    }
    for chunk in chunks {
        out.extend_from_slice(&chunk.compressed);
    }
    Ok(out)
}

fn append_chunk(
    members: &mut [EncodedMember],
    files: &BTreeMap<String, Vec<u8>>,
    indices: &[usize],
    chunks: &mut Vec<EncodedChunk>,
) -> Result<()> {
    let chunk_index = u32_len(chunks.len(), "chunk index")?;
    let raw_len: usize = indices.iter().try_fold(0usize, |total, &index| {
        total
            .checked_add(files[&members[index].identity.name].len())
            .ok_or_else(|| anyhow!("prepared-package chunk length overflow"))
    })?;
    let mut raw = Vec::with_capacity(raw_len);
    for &index in indices {
        members[index].chunk = chunk_index;
        members[index].raw_offset = raw.len() as u64;
        raw.extend_from_slice(&files[&members[index].identity.name]);
    }
    let raw_sha256 = Sha256::digest(&raw).into();
    chunks.push(EncodedChunk {
        raw_len: raw.len() as u64,
        raw_sha256,
        compressed: deflate::compress_to_vec(&raw, COMPRESSION_LEVEL),
    });
    Ok(())
}

#[derive(Clone)]
struct DecodedMember {
    identity: MemberIdentity,
    chunk: usize,
    raw_offset: u64,
}

fn decode_shared(
    backing: PreparedArtifactBacking,
    artifact_range: Range<usize>,
    expected: Option<&PreparedPackageKey>,
) -> Result<PreparedPackage> {
    let bytes = backing
        .0
        .bytes
        .get(artifact_range.clone())
        .ok_or_else(|| anyhow!("prepared-package artifact range is out of bounds"))?;
    if bytes.len() < MAGIC.len() + 4 * 4 + 32 {
        bail!("prepared-package artifact is truncated");
    }
    let artifact_sha256 = hex::encode(Sha256::digest(bytes));
    let artifact = ExactArtifact {
        bytes: if artifact_range.start == 0 && artifact_range.end == backing.0.bytes.len() {
            backing.0.bytes.clone()
        } else {
            Rc::new(bytes.to_vec())
        },
        sha256: artifact_sha256,
    };

    let mut reader = Reader::new(bytes);
    if reader.take(MAGIC.len())? != MAGIC {
        bail!("prepared-package artifact has the wrong magic");
    }
    let format_version = reader.u32()?;
    let normalization_version = reader.u32()?;
    let derived_index_version = reader.u32()?;
    let engine_abi_version = reader.u32()?;
    let source_sha256 = hex::encode(reader.take(32)?);
    let key = PreparedPackageKey {
        format_version,
        normalization_version,
        derived_index_version,
        engine_abi_version,
        source_sha256,
    };
    key.require_current()?;
    if expected.is_some_and(|expected| expected != &key) {
        bail!("prepared-package embedded key does not match the requested cache key");
    }
    let label = reader.counted_string("package label")?;
    parse_exact_package_label(&label)?;

    let dependency_count = reader.u32()? as usize;
    let mut declared_dependencies = BTreeMap::new();
    let mut previous_dependency: Option<String> = None;
    for _ in 0..dependency_count {
        let name = reader.counted_string("dependency name")?;
        let version = reader.counted_string("dependency version")?;
        if name.is_empty() || version.is_empty() {
            bail!("prepared-package dependency name/version must not be empty");
        }
        if previous_dependency
            .as_ref()
            .is_some_and(|prior| prior >= &name)
        {
            bail!("prepared-package dependencies are not in canonical unique order");
        }
        previous_dependency = Some(name.clone());
        declared_dependencies.insert(name, version);
    }

    let file_count = reader.u32()? as usize;
    let chunk_count = reader.u32()? as usize;
    if file_count == 0 || chunk_count == 0 {
        bail!("prepared-package must contain members and chunks");
    }
    // Reject impossible attacker-controlled counts before using them as Vec
    // capacities. Even the smallest valid member consumes 57 directory bytes
    // (a one-byte name plus fixed fields), and every chunk consumes 48 bytes.
    // Exact carrier integrity comes from the cached SHA-256 ContentRef, so the
    // parser must still reject impossible attacker-controlled allocation counts
    // before using them as Vec capacities.
    let minimum_directory_bytes = file_count
        .checked_mul(57)
        .and_then(|bytes| {
            chunk_count
                .checked_mul(48)
                .and_then(|chunks| bytes.checked_add(chunks))
        })
        .ok_or_else(|| anyhow!("prepared-package directory size overflow"))?;
    if minimum_directory_bytes > reader.remaining() {
        bail!("prepared-package member/chunk counts exceed the artifact directory");
    }
    let mut members = Vec::with_capacity(file_count);
    let mut previous: Option<String> = None;
    for _ in 0..file_count {
        let name = reader.counted_string("member name")?;
        validate_member_name(&name)?;
        if previous.as_ref().is_some_and(|prior| prior >= &name) {
            bail!("prepared-package members are not in canonical unique order");
        }
        previous = Some(name.clone());
        let chunk = reader.u32()? as usize;
        let raw_offset = reader.u64()?;
        let raw_len = reader.u64()?;
        let raw_sha256 = reader.take(32)?.try_into().unwrap();
        members.push(DecodedMember {
            identity: MemberIdentity {
                name,
                raw_len,
                raw_sha256,
            },
            chunk,
            raw_offset,
        });
    }
    if !members
        .iter()
        .any(|member| member.identity.name == "package.json")
    {
        bail!("prepared package has no package.json member");
    }
    if !members
        .iter()
        .any(|member| member.identity.name == derived_index::SIDECAR_NAME)
    {
        bail!("prepared package has no current derived-index sidecar member");
    }

    let actual_source = source_metadata_digest(
        &label,
        &declared_dependencies,
        members
            .iter()
            .map(|member| &member.identity)
            .filter(|member| member.name != derived_index::SIDECAR_NAME),
    );
    if actual_source != key.source_digest()? {
        bail!("prepared-package source metadata digest mismatch");
    }
    #[derive(Clone)]
    struct ChunkDescription {
        raw_len: usize,
        compressed_len: usize,
        raw_sha256: [u8; 32],
    }
    let mut descriptions = Vec::with_capacity(chunk_count);
    for _ in 0..chunk_count {
        let raw_len: usize = reader
            .u64()?
            .try_into()
            .map_err(|_| anyhow!("prepared-package chunk is too large for this host"))?;
        let compressed_len: usize = reader
            .u64()?
            .try_into()
            .map_err(|_| anyhow!("prepared-package chunk is too large for this host"))?;
        descriptions.push(ChunkDescription {
            raw_len,
            compressed_len,
            raw_sha256: reader.take(32)?.try_into().unwrap(),
        });
    }

    let mut chunk_ranges = Vec::with_capacity(chunk_count);
    for description in &descriptions {
        let relative_start = reader.position();
        reader.take(description.compressed_len)?;
        let absolute_start = artifact_range
            .start
            .checked_add(relative_start)
            .ok_or_else(|| anyhow!("prepared-package compressed range overflow"))?;
        let absolute_end = absolute_start
            .checked_add(description.compressed_len)
            .ok_or_else(|| anyhow!("prepared-package compressed range overflow"))?;
        chunk_ranges.push(absolute_start..absolute_end);
    }
    if !reader.is_empty() {
        bail!("prepared-package artifact has trailing data");
    }

    let mut grouped: Vec<Vec<&DecodedMember>> = vec![Vec::new(); chunk_count];
    for member in &members {
        let group = grouped
            .get_mut(member.chunk)
            .ok_or_else(|| anyhow!("prepared-package member references missing chunk"))?;
        group.push(member);
    }
    for (index, group) in grouped.iter_mut().enumerate() {
        if group.is_empty() {
            bail!("prepared-package contains an unreferenced chunk");
        }
        group.sort_by_key(|member| member.raw_offset);
        let mut cursor = 0u64;
        for member in group {
            if member.raw_offset != cursor {
                bail!("prepared-package chunk members contain a gap or overlap");
            }
            cursor = cursor
                .checked_add(member.identity.raw_len)
                .ok_or_else(|| anyhow!("prepared-package raw member range overflow"))?;
        }
        if cursor != descriptions[index].raw_len as u64 {
            bail!("prepared-package chunk members do not partition the raw chunk");
        }
    }

    let chunks: Vec<_> = descriptions
        .iter()
        .zip(chunk_ranges)
        .map(|(description, compressed)| {
            Rc::new(CompressedChunk {
                compressed,
                raw_len: description.raw_len,
                raw_sha256: description.raw_sha256,
            })
        })
        .collect();
    let mut compressed_members = BTreeMap::new();
    for member in members {
        let start: usize = member
            .raw_offset
            .try_into()
            .map_err(|_| anyhow!("prepared-package member offset is too large for this host"))?;
        let length: usize = member
            .identity
            .raw_len
            .try_into()
            .map_err(|_| anyhow!("prepared-package member is too large for this host"))?;
        let end = start
            .checked_add(length)
            .ok_or_else(|| anyhow!("prepared-package member range overflow"))?;
        compressed_members.insert(
            member.identity.name,
            CompressedMember {
                chunk: Rc::clone(&chunks[member.chunk]),
                raw: start..end,
                raw_sha256: member.identity.raw_sha256,
            },
        );
    }

    Ok(PreparedPackage {
        label,
        key,
        files: PreparedFiles(PreparedFileStorage::Compressed(PreparedCompressedFiles {
            backing,
            members: compressed_members,
            artifact_range,
        })),
        declared_dependencies,
        artifact: OnceCell::from(artifact),
    })
}

fn identities_from_files(files: &BTreeMap<String, Vec<u8>>) -> Vec<MemberIdentity> {
    files
        .iter()
        .map(|(name, body)| MemberIdentity {
            name: name.clone(),
            raw_len: body.len() as u64,
            raw_sha256: Sha256::digest(body).into(),
        })
        .collect()
}

fn source_metadata_digest<'a>(
    label: &str,
    dependencies: &BTreeMap<String, String>,
    members: impl IntoIterator<Item = &'a MemberIdentity>,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(SOURCE_ID_DOMAIN);
    hasher.update((label.len() as u64).to_be_bytes());
    hasher.update(label.as_bytes());
    hasher.update((dependencies.len() as u64).to_be_bytes());
    for (name, version) in dependencies {
        hasher.update((name.len() as u64).to_be_bytes());
        hasher.update(name.as_bytes());
        hasher.update((version.len() as u64).to_be_bytes());
        hasher.update(version.as_bytes());
    }
    for member in members {
        hasher.update((member.name.len() as u64).to_be_bytes());
        hasher.update(member.name.as_bytes());
        hasher.update(member.raw_len.to_be_bytes());
        hasher.update(member.raw_sha256);
    }
    hasher.finalize().into()
}

fn u32_len(value: usize, field: &str) -> Result<u32> {
    value
        .try_into()
        .with_context(|| format!("prepared-package {field} exceeds u32"))
}

fn put_string(out: &mut Vec<u8>, value: &str) -> Result<()> {
    put_u32(out, u32_len(value.len(), "string length")?);
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| anyhow!("prepared-package length overflow"))?;
        if end > self.bytes.len() {
            bail!("prepared-package artifact is truncated");
        }
        let value = &self.bytes[self.position..end];
        self.position = end;
        Ok(value)
    }

    fn position(&self) -> usize {
        self.position
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn counted_string(&mut self, field: &str) -> Result<String> {
        let length = self.u32()? as usize;
        String::from_utf8(self.take(length)?.to_vec())
            .with_context(|| format!("prepared-package {field} is not UTF-8"))
    }

    fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PackageSource;

    fn entries() -> BTreeMap<String, Vec<u8>> {
        BTreeMap::from([
            (
                "package.json".into(),
                br#"{"name":"example.pkg","version":"1.2.3","dependencies":{"dep.pkg":"2.0.0"}}"#
                    .to_vec(),
            ),
            (
                "StructureDefinition-p.json".into(),
                br#"{"resourceType":"StructureDefinition","id":"p","name":"P"}"#.repeat(4000),
            ),
            ("template/config.json".into(), br#"{"x":1}"#.to_vec()),
        ])
    }

    #[test]
    fn deterministic_compact_round_trip_and_lazy_mount() {
        let input = entries();
        let prepared = PreparedPackage::prepare("example.pkg#1.2.3", input).unwrap();
        assert_eq!(
            prepared.key.cache_key(),
            format!(
                "pp3-sha256-{}-n1-d{}-a1",
                prepared.key.source_sha256,
                derived_index::DERIVED_INDEX_FORMAT_VERSION
            )
        );
        let expanded: usize = prepared
            .files
            .metadata()
            .iter()
            .map(|member| member.raw_len as usize)
            .sum();
        let encoded = prepared.encode();
        let expected_artifact_sha256 = hex::encode(Sha256::digest(&encoded));
        assert_eq!(prepared.artifact_sha256(), expected_artifact_sha256);
        assert_eq!(encoded, prepared.encode());
        assert!(encoded.len() < expanded / 4);

        let decoded = PreparedPackage::decode_expected(&encoded, &prepared.key).unwrap();
        assert!(decoded.files.is_blob_backed());
        assert_eq!(decoded, prepared);
        assert_eq!(decoded.encode(), encoded);
        assert_eq!(decoded.artifact_bytes().as_slice(), encoded);
        assert_eq!(decoded.artifact_sha256(), expected_artifact_sha256);
        assert_eq!(decoded.declared_dependencies["dep.pkg"], "2.0.0");

        // Structural decode/mount did not create a decompressed cache entry.
        let PreparedFileStorage::Compressed(compressed) = &decoded.files.0 else {
            panic!("decoded files are not compressed")
        };
        assert!(Rc::ptr_eq(
            &decoded.artifact_bytes(),
            &compressed.backing.0.bytes
        ));
        assert!(compressed.backing.0.cache.borrow().entries.is_empty());

        let mut source = BundleSource::new();
        let mounted = decoded.mount_into(&mut source);
        assert_eq!(mounted.artifact_bytes().as_slice(), encoded);
        assert_eq!(mounted.artifact_sha256(), expected_artifact_sha256);
        let package_dir = source.cache_root().join(&mounted.label).join("package");
        let before = source.compression_metrics();
        assert!(before.compressed_retained_bytes > 0);
        assert!(before.declared_raw_bytes as usize >= expanded);
        assert_eq!(before.chunks_inflated, 0);
        assert_eq!(before.cached_raw_bytes, 0);
        assert!(source.exists(&package_dir.join("StructureDefinition-p.json")));
        assert_eq!(
            source
                .read(&package_dir.join("template/config.json"))
                .unwrap(),
            br#"{"x":1}"#
        );
        let after_first = source.compression_metrics();
        assert_eq!(after_first.chunks_inflated, 1);
        assert!(after_first.raw_inflated_bytes > 0);
        assert!(after_first.cached_raw_bytes > 0);
        source
            .read(&package_dir.join("template/config.json"))
            .unwrap();
        assert_eq!(source.compression_metrics().cache_hits, 1);
        assert_eq!(
            derived_index::load(&source, &package_dir),
            derived_index::parse(
                &source
                    .read(&package_dir.join(derived_index::SIDECAR_NAME))
                    .unwrap()
            )
            .unwrap()
        );
    }

    #[test]
    fn batch_backing_shares_bytes_and_verified_chunk_cache() {
        let prepared = PreparedPackage::prepare("example.pkg#1.2.3", entries()).unwrap();
        let artifact = prepared.encode();
        let mut batch = artifact.clone();
        batch.extend_from_slice(&artifact);
        let backing = PreparedArtifactBacking::new(batch);
        let first = PreparedPackage::decode_backing_range(
            backing.clone(),
            0..artifact.len(),
            &prepared.key,
        )
        .unwrap();
        let second = PreparedPackage::decode_backing_range(
            backing,
            artifact.len()..artifact.len() * 2,
            &prepared.key,
        )
        .unwrap();
        assert!(first.files.shares_backing_blob_with(&second.files));
        assert_eq!(first, second);
        assert!(first.files.read("package.json").unwrap().is_some());
    }

    #[test]
    fn rejects_stale_keys_and_wrong_identity() {
        let prepared = PreparedPackage::prepare("example.pkg#1.2.3", entries()).unwrap();
        let key_text = prepared.key.cache_key();
        assert_eq!(
            key_text.parse::<PreparedPackageKey>().unwrap(),
            prepared.key
        );
        let mut other_entries = entries();
        other_entries.insert("extra.txt".into(), b"extra".to_vec());
        let other = PreparedPackage::prepare("example.pkg#1.2.3", other_entries).unwrap();
        assert!(PreparedPackage::decode_expected(&prepared.encode(), &other.key).is_err());
        assert!(PreparedPackage::prepare("other.pkg#1.2.3", entries()).is_err());

        let uppercase_digest = prepared.key.cache_key().replacen(
            &prepared.key.source_sha256,
            &prepared.key.source_sha256.to_ascii_uppercase(),
            1,
        );
        assert!(uppercase_digest
            .parse::<PreparedPackageKey>()
            .unwrap_err()
            .to_string()
            .contains("canonical lowercase hex"));
    }

    #[test]
    fn metadata_roots_reject_changed_member_identity_and_body_checks_are_lazy() {
        let source = entries();
        let prepared = PreparedPackage::prepare("example.pkg#1.2.3", source.clone()).unwrap();
        let encoded = prepared.encode();

        let member_sha = Sha256::digest(&source["template/config.json"]);
        let mut forged_metadata = encoded.clone();
        let metadata_sha_offset = forged_metadata
            .windows(32)
            .position(|window| window == member_sha.as_slice())
            .expect("member SHA is present in the canonical directory");
        forged_metadata[metadata_sha_offset] ^= 1;
        assert!(
            PreparedPackage::decode_expected(&forged_metadata, &prepared.key)
                .unwrap_err()
                .to_string()
                .contains("source metadata digest")
        );

        let mut forged_label = encoded.clone();
        let label_offset = forged_label
            .windows(b"example.pkg#1.2.3".len())
            .position(|window| window == b"example.pkg#1.2.3")
            .expect("package label is present in the header");
        forged_label[label_offset] = b'f';
        assert!(
            PreparedPackage::decode_expected(&forged_label, &prepared.key)
                .unwrap_err()
                .to_string()
                .contains("source metadata digest")
        );

        let decoded = PreparedPackage::decode_expected(&encoded, &prepared.key).unwrap();
        let original_sha256 = decoded.artifact_sha256().to_string();
        let PreparedFileStorage::Compressed(files) = decoded.files.0 else {
            panic!("decoded files are not compressed")
        };
        let compressed_offset = files.members["template/config.json"].chunk.compressed.start;
        let mut forged_body = encoded;
        forged_body[compressed_offset] ^= 1;
        let decoded = PreparedPackage::decode_expected(&forged_body, &prepared.key).unwrap();
        assert_ne!(decoded.artifact_sha256(), original_sha256);
        assert!(decoded.files.read("template/config.json").is_err());
    }

    #[test]
    fn bounded_inflate_rejects_a_chunk_that_expands_past_its_declaration() {
        let prepared = PreparedPackage::prepare("example.pkg#1.2.3", entries()).unwrap();
        let encoded = prepared.encode();
        let decoded = PreparedPackage::decode_expected(&encoded, &prepared.key).unwrap();
        let PreparedFileStorage::Compressed(files) = decoded.files.0 else {
            panic!("decoded files are not compressed")
        };
        let member = files.members["package.json"].clone();
        let forged = CompressedChunk {
            raw_len: member.chunk.raw_len.saturating_sub(1),
            ..(*member.chunk).clone()
        };
        let error = files.backing.0.read_chunk(&forged).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_impossible_counts_before_allocating_directory_vectors() {
        let prepared = PreparedPackage::prepare("example.pkg#1.2.3", entries()).unwrap();
        let mut encoded = prepared.encode();
        let mut reader = Reader::new(&encoded);
        reader.take(MAGIC.len() + 4 * 4 + 32).unwrap();
        reader.counted_string("package label").unwrap();
        let dependencies = reader.u32().unwrap();
        for _ in 0..dependencies {
            reader.counted_string("dependency name").unwrap();
            reader.counted_string("dependency version").unwrap();
        }
        let file_count_offset = reader.position();
        encoded[file_count_offset..file_count_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        let error = PreparedPackage::decode_expected(&encoded, &prepared.key).unwrap_err();
        assert!(error
            .to_string()
            .contains("member/chunk counts exceed the artifact directory"));
    }
}
