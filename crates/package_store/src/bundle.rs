//! `BundleSource` — a read-only, in-memory [`PackageSource`] that mounts a set of
//! prebuilt package bundles.
//!
//! This is the package-source shape the browser mounts. Cold material arrives as
//! one fetched compressed carrier per package and is converted directly to a
//! PreparedPackage v3 artifact without an aggregate raw map. The legacy native
//! acquisition helper still returns an owned map. Mounted v3 material keeps
//! compact chunks and inflates members lazily.
//! Neither path needs `std::fs`, so both compile and run on
//! `wasm32-unknown-unknown`. Native tests exercise the source directly (the P1
//! BundleSource fixture-ladder gate).
//!
//! # Bundle format (v1)
//!
//! A *package bundle* is the byte content of one materialized `package/`
//! directory: every resource JSON plus the stock `.index.json` and the derived
//! `.derived-index.json` sidecar. The on-the-wire container is a **gzipped tar**
//! whose entries are the package-relative file names (`StructureDefinition-*.json`,
//! `.index.json`, `.derived-index.json`, `package.json`, …) — i.e. exactly what
//! the read path lists and reads. The builder (`package_acquisition`) emits one
//! such blob per package and a [`BundleManifest`] lockfile pinning the set.
//!
//! [`BundleSource`] itself is container-agnostic: cold material uses owned
//! `path -> bytes` maps, while prepared material retains a compact raw-DEFLATE
//! chunk store plus a validated member directory. Both use the same cache paths
//! (`<cache>/<id>#<ver>/package/<file>`). [`BundleSource::mount_package`] takes a
//! package's already-inflated `file -> bytes` entries and places them under that
//! package's dir. Prepared-package chunks use the pure-Rust `miniz_oxide`
//! decoder, which is also used by the wasm32 build.

use crate::prepared::{
    encode_from_member_bodies, MemberIdentity, PreparedCompressedFiles, PreparedPackageKey,
};
use crate::source::{DirEntry, PackageSource};
use crate::{derived_index, material};
use anyhow::{anyhow, bail, Context};
use flate2::bufread::GzDecoder as BufferedGzDecoder;
use flate2::read::{DeflateDecoder, GzDecoder};
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// The bundle format version. Bump on any incompatible change to the container or
/// manifest shape so a reader can reject a stale bundle.
pub const BUNDLE_FORMAT_VERSION: u32 = 1;

/// Ceilings for the legacy aggregate-map TGZ reader used by native package
/// acquisition. Browser cold preparation uses the independent streaming limits
/// below because legitimate terminology packages can exceed this aggregate
/// expanded-byte limit.
pub const MAX_PACKAGE_TGZ_MEMBERS: u64 = 65_536;
pub const MAX_PACKAGE_TGZ_MEMBER_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_PACKAGE_TGZ_EXPANDED_BYTES: u64 = 256 * 1024 * 1024;

/// Streaming cold preparation accepts large legitimate terminology packages
/// without retaining their aggregate raw form. The logical, metadata, member,
/// and count limits are hard per-carrier work bounds. The working-set value is
/// a conservative estimate of bytes live inside one preparation call; it is
/// not a whole-engine, allocator-RSS, or Chromium-process guarantee.
pub const MAX_STREAMING_TGZ_LOGICAL_BYTES: u64 = 1536 * 1024 * 1024;
pub const MAX_BROWSER_PREPARATION_WORKING_SET_BYTES: u64 = 640 * 1024 * 1024;
pub const MAX_STREAMING_TGZ_METADATA_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_STREAMING_TGZ_EXTENSION_RECORDS: u64 = 16_384;
pub const MAX_STREAMING_TGZ_EXTENSION_BYTES: u64 = 1024 * 1024;
const MAX_STREAMING_EXPANSION_RATIO: u64 = 64;
const STREAMING_RATIO_ALLOWANCE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy)]
struct StreamingTgzLimits {
    logical_bytes: u64,
    working_set_bytes: u64,
    metadata_bytes: u64,
    extension_records: u64,
    extension_bytes: u64,
    expansion_ratio: u64,
    ratio_allowance_bytes: u64,
}

const STREAMING_TGZ_LIMITS: StreamingTgzLimits = StreamingTgzLimits {
    logical_bytes: MAX_STREAMING_TGZ_LOGICAL_BYTES,
    working_set_bytes: MAX_BROWSER_PREPARATION_WORKING_SET_BYTES,
    metadata_bytes: MAX_STREAMING_TGZ_METADATA_BYTES,
    extension_records: MAX_STREAMING_TGZ_EXTENSION_RECORDS,
    extension_bytes: MAX_STREAMING_TGZ_EXTENSION_BYTES,
    expansion_ratio: MAX_STREAMING_EXPANSION_RATIO,
    ratio_allowance_bytes: STREAMING_RATIO_ALLOWANCE_BYTES,
};

#[derive(Debug)]
struct PackageResourcePolicyExceeded {
    message: String,
}

impl fmt::Display for PackageResourcePolicyExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PackageResourcePolicyExceeded {}

fn resource_policy_error(message: impl Into<String>) -> anyhow::Error {
    PackageResourcePolicyExceeded {
        message: message.into(),
    }
    .into()
}

fn resource_policy_io_error(message: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::Other,
        PackageResourcePolicyExceeded {
            message: message.into(),
        },
    )
}

/// Whether streaming preparation stopped because this carrier exceeded the
/// configured per-call browser resource policy. This classification says
/// nothing about package validity and is not a whole-process/RSS guarantee.
pub fn is_package_resource_policy_exhaustion(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<PackageResourcePolicyExceeded>()
            .is_some()
            || cause
                .downcast_ref::<io::Error>()
                .and_then(io::Error::get_ref)
                .and_then(|inner| inner.downcast_ref::<PackageResourcePolicyExceeded>())
                .is_some()
    })
}

/// Exact v3 artifact produced from a raw TGZ without an aggregate raw file map.
#[derive(Debug)]
pub struct PreparedTgzArtifact {
    pub key: PreparedPackageKey,
    pub artifact: Vec<u8>,
    pub members: u64,
    pub logical_raw_bytes: u64,
    pub canonical_raw_bytes: u64,
}

struct SpooledMember {
    identity: MemberIdentity,
    compressed: Vec<u8>,
}

#[derive(Default)]
struct PendingTarExtensions {
    long_name: Option<Vec<u8>>,
    long_link: bool,
    pax_local: bool,
    pax_path: Option<Vec<u8>>,
    pax_size: Option<u64>,
}

impl PendingTarExtensions {
    fn is_empty(&self) -> bool {
        self.long_name.is_none()
            && !self.long_link
            && !self.pax_local
            && self.pax_path.is_none()
            && self.pax_size.is_none()
    }
}

/// Count every byte exposed by the decompressed gzip stream, including tar
/// headers, padding, GNU/PAX extension bodies, and bytes after tar EOF. This
/// wrapper sits below `tar::Archive`, so its ceiling applies before the tar
/// crate can allocate an extension body internally.
struct CountedTarRead<R> {
    inner: R,
    observed: u64,
    maximum: u64,
}

impl<R> CountedTarRead<R> {
    fn new(inner: R, maximum: u64) -> Self {
        Self {
            inner,
            observed: 0,
            maximum,
        }
    }

    fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for CountedTarRead<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let remaining = self.maximum.saturating_sub(self.observed);
        let allowed = usize::try_from(remaining.saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let count = self.inner.read(&mut buffer[..allowed])?;
        self.observed = self
            .observed
            .checked_add(count as u64)
            .ok_or_else(|| resource_policy_io_error("package TGZ work estimate overflowed"))?;
        if self.observed > self.maximum {
            return Err(resource_policy_io_error(format!(
                "browser preparation resource policy allows {} decompressed bytes for this carrier",
                self.maximum
            )));
        }
        Ok(count)
    }
}

#[derive(Clone, Copy)]
struct PackageTgzLimits {
    members: u64,
    member_bytes: u64,
    expanded_bytes: u64,
}

const PACKAGE_TGZ_LIMITS: PackageTgzLimits = PackageTgzLimits {
    members: MAX_PACKAGE_TGZ_MEMBERS,
    member_bytes: MAX_PACKAGE_TGZ_MEMBER_BYTES,
    expanded_bytes: MAX_PACKAGE_TGZ_EXPANDED_BYTES,
};

/// Inflate one npm/FHIR `.tgz` into its package-relative regular files. This is
/// the single native/WASM carrier parser: package acquisition and browser cold
/// preparation therefore agree on root and path normalization. Identity,
/// dependencies, and derived indexes remain the PreparedPackage boundary's job.
pub fn read_package_tgz(bytes: &[u8]) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    read_package_tgz_with_limits(bytes, PACKAGE_TGZ_LIMITS)
}

/// Convert one untrusted FHIR package TGZ directly into the canonical
/// PreparedPackage v3 artifact. Tar members are validated and compressed one at
/// a time, then replayed in BTreeMap order through the same v3 encoder used by
/// map-backed preparation. Aggregate raw bytes are never retained.
pub fn prepare_package_tgz_streaming(
    label: &str,
    bytes: &[u8],
) -> anyhow::Result<PreparedTgzArtifact> {
    prepare_package_tgz_streaming_with_limits(label, bytes, STREAMING_TGZ_LIMITS)
}

fn prepare_package_tgz_streaming_with_limits(
    label: &str,
    bytes: &[u8],
    limits: StreamingTgzLimits,
) -> anyhow::Result<PreparedTgzArtifact> {
    let input_bytes = u64::try_from(bytes.len()).context("TGZ input length exceeds u64")?;
    require_browser_working_set_budget(input_bytes, 0, 0, 0, 0, 0, limits.working_set_bytes)?;
    let ratio_limit = input_bytes
        .checked_mul(limits.expansion_ratio)
        .and_then(|value| value.checked_add(limits.ratio_allowance_bytes))
        .unwrap_or(u64::MAX)
        .min(limits.logical_bytes);
    let decoder = BufferedGzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(CountedTarRead::new(decoder, ratio_limit));
    let mut seen_names = BTreeSet::new();
    let mut spooled = BTreeMap::new();
    let mut derived = BTreeMap::new();
    let mut dependencies = None;
    let mut archive_entries = 0u64;
    let mut logical_raw_bytes = 0u64;
    let mut spool_bytes = 0u64;
    let mut metadata_bytes = 0u64;
    let mut extension_records = 0u64;
    let mut pending_extensions = PendingTarExtensions::default();

    // Raw iteration is deliberate: normal tar iteration reads GNU/PAX bodies
    // into hidden Vecs before returning the described entry. Surfacing those
    // records lets this boundary reject/count them before retaining any body.
    for candidate in archive
        .entries()
        .context("read gzip/tar entries")?
        .raw(true)
    {
        let mut entry = candidate.context("read tar entry")?;
        archive_entries = archive_entries
            .checked_add(1)
            .ok_or_else(|| anyhow!("package bundle member count overflow"))?;
        if archive_entries > MAX_PACKAGE_TGZ_MEMBERS {
            return Err(resource_policy_error(format!(
                "browser preparation resource policy allows at most {} tar members",
                MAX_PACKAGE_TGZ_MEMBERS
            )));
        }
        let member_bytes = entry.size();
        if member_bytes > MAX_PACKAGE_TGZ_MEMBER_BYTES {
            return Err(resource_policy_error(format!(
                "browser preparation resource policy allows at most {} bytes in one tar member; this member declares {member_bytes} bytes",
                MAX_PACKAGE_TGZ_MEMBER_BYTES
            )));
        }
        logical_raw_bytes = logical_raw_bytes
            .checked_add(member_bytes)
            .ok_or_else(|| anyhow!("package bundle expanded byte count overflow"))?;
        if logical_raw_bytes > ratio_limit {
            return Err(resource_policy_error(format!(
                "browser preparation resource policy allows {ratio_limit} logical member bytes for this carrier"
            )));
        }
        let entry_type = entry.header().entry_type();
        let recognized_header =
            entry.header().as_gnu().is_some() || entry.header().as_ustar().is_some();
        let is_local_extension = recognized_header
            && (entry_type.is_gnu_longname()
                || entry_type.is_gnu_longlink()
                || entry_type.is_pax_local_extensions());
        if is_local_extension || entry_type.is_pax_global_extensions() {
            extension_records = extension_records
                .checked_add(1)
                .ok_or_else(|| anyhow!("package bundle extension record count overflow"))?;
            if extension_records > limits.extension_records {
                return Err(resource_policy_error(format!(
                    "browser preparation resource policy allows at most {} tar extension records",
                    limits.extension_records
                )));
            }
            if member_bytes > limits.extension_bytes {
                return Err(resource_policy_error(format!(
                    "browser preparation resource policy allows at most {} bytes in one tar extension record; this record declares {member_bytes} bytes",
                    limits.extension_bytes
                )));
            }
            metadata_bytes = metadata_bytes
                .checked_add(member_bytes)
                .and_then(|value| value.checked_add(64))
                .ok_or_else(|| anyhow!("package bundle metadata byte count overflow"))?;
            if metadata_bytes > limits.metadata_bytes {
                return Err(resource_policy_error(format!(
                    "browser preparation resource policy allows {} bytes of retained package metadata",
                    limits.metadata_bytes
                )));
            }
            require_browser_working_set_budget(
                input_bytes,
                metadata_bytes,
                spool_bytes,
                0,
                member_bytes,
                0,
                limits.working_set_bytes,
            )?;

            if entry_type.is_pax_global_extensions() {
                if !pending_extensions.is_empty() {
                    bail!("global PAX header interrupts local tar extension sequence");
                }
                drain_tar_entry(&mut entry).context("drain global PAX extension")?;
                continue;
            }
            if entry_type.is_gnu_longlink() {
                if pending_extensions.long_link {
                    bail!("two GNU long-link records describe one package member");
                }
                pending_extensions.long_link = true;
                drain_tar_entry(&mut entry).context("drain GNU long-link extension")?;
                continue;
            }

            let body = read_bounded_tar_entry(&mut entry, member_bytes)
                .context("read package tar extension")?;
            if entry_type.is_gnu_longname() {
                if pending_extensions.long_name.is_some() {
                    bail!("two GNU long-name records describe one package member");
                }
                pending_extensions.long_name = Some(trim_gnu_long_name(body));
            } else {
                if pending_extensions.pax_local {
                    bail!("two local PAX records describe one package member");
                }
                pending_extensions.pax_local = true;
                let (path, size) = parse_local_pax(&body)?;
                pending_extensions.pax_path = path;
                pending_extensions.pax_size = size;
            }
            continue;
        }

        let extensions = std::mem::take(&mut pending_extensions);
        if let Some(pax_size) = extensions.pax_size {
            if pax_size != member_bytes {
                bail!(
                    "PAX size override {pax_size} disagrees with raw tar member size {member_bytes}"
                );
            }
        }
        if !entry_type.is_file() {
            continue;
        }
        let header_path = entry.header().path_bytes();
        let raw_name_bytes = extensions
            .long_name
            .as_deref()
            .or(extensions.pax_path.as_deref())
            .unwrap_or(header_path.as_ref());
        let raw_name = std::str::from_utf8(raw_name_bytes)
            .map_err(|_| anyhow!("package bundle member name is not UTF-8"))?
            .to_string();
        let name = normalized_tgz_member_name(&raw_name)?;
        if !seen_names.insert(name.clone()) {
            bail!("duplicate package bundle member after root normalization: {name}");
        }
        metadata_bytes = metadata_bytes
            .checked_add(name.len() as u64)
            .and_then(|value| value.checked_add(64))
            .ok_or_else(|| anyhow!("package bundle metadata byte count overflow"))?;
        if metadata_bytes > limits.metadata_bytes {
            return Err(resource_policy_error(format!(
                "browser preparation resource policy allows {} bytes of retained package metadata",
                limits.metadata_bytes
            )));
        }

        let member_len = usize::try_from(member_bytes)
            .map_err(|_| anyhow!("package bundle member is too large for this host"))?;
        require_browser_working_set_budget(
            input_bytes,
            metadata_bytes,
            spool_bytes,
            0,
            member_bytes,
            0,
            limits.working_set_bytes,
        )?;
        let mut body = Vec::new();
        body.try_reserve_exact(member_len)
            .with_context(|| format!("reserve package bundle member {name}"))?;
        entry
            .read_to_end(&mut body)
            .with_context(|| format!("read package bundle member {name}"))?;
        if body.len() != member_len {
            bail!(
                "package bundle member {name} declared {member_len} bytes but yielded {}",
                body.len()
            );
        }
        require_browser_working_set_budget(
            input_bytes,
            metadata_bytes,
            spool_bytes,
            0,
            body.capacity() as u64,
            0,
            limits.working_set_bytes,
        )?;

        // A shipped sidecar is deliberately non-authoritative. It still counts
        // toward transport work and duplicate detection, but canonical output
        // always regenerates it from current top-level resource bytes.
        if name == derived_index::SIDECAR_NAME {
            continue;
        }
        if name == "package.json" {
            metadata_bytes = metadata_bytes
                .checked_add(body.len() as u64)
                .ok_or_else(|| anyhow!("package bundle metadata byte count overflow"))?;
            if metadata_bytes > limits.metadata_bytes {
                return Err(resource_policy_error(format!(
                    "browser preparation resource policy allows {} bytes of retained package metadata",
                    limits.metadata_bytes
                )));
            }
            let parsed = material::validate_package_identity_bytes(label, &body)?;
            metadata_bytes = parsed
                .iter()
                .try_fold(metadata_bytes, |total, (name, version)| {
                    total
                        .checked_add(name.len() as u64)
                        .and_then(|value| value.checked_add(version.len() as u64))
                        .and_then(|value| value.checked_add(64))
                        .ok_or_else(|| anyhow!("package bundle metadata byte count overflow"))
                })?;
            if metadata_bytes > limits.metadata_bytes {
                return Err(resource_policy_error(format!(
                    "browser preparation resource policy allows {} bytes of retained package metadata",
                    limits.metadata_bytes
                )));
            }
            dependencies = Some(parsed);
        } else if is_derived_index_resource_candidate(&name) {
            if let Some(row) = derived_index::entry_from_package_bytes(name.clone(), &body) {
                metadata_bytes = metadata_bytes
                    .checked_add(row.retained_bytes())
                    .ok_or_else(|| anyhow!("package bundle metadata byte count overflow"))?;
                if metadata_bytes > limits.metadata_bytes {
                    return Err(resource_policy_error(format!(
                        "browser preparation resource policy allows {} bytes of retained package metadata",
                        limits.metadata_bytes
                    )));
                }
                derived.insert(name.clone(), row);
            }
        }
        let compression_bound = temporary_deflate_capacity_bound(body.len() as u64)?;
        require_browser_working_set_budget(
            input_bytes,
            metadata_bytes,
            spool_bytes,
            0,
            body.capacity() as u64,
            compression_bound,
            limits.working_set_bytes,
        )?;
        let compressed = compress_temporary_member(&body)
            .with_context(|| format!("compress package bundle member {name}"))?;
        spool_bytes = spool_bytes
            .checked_add(compressed.capacity() as u64)
            .ok_or_else(|| anyhow!("package bundle temporary spool byte count overflow"))?;
        require_browser_working_set_budget(
            input_bytes,
            metadata_bytes,
            spool_bytes,
            0,
            body.capacity() as u64,
            0,
            limits.working_set_bytes,
        )?;
        let identity = MemberIdentity {
            name: name.clone(),
            raw_len: body.len() as u64,
            raw_sha256: Sha256::digest(&body).into(),
        };
        spooled.insert(
            name,
            SpooledMember {
                identity,
                compressed,
            },
        );
    }
    if !pending_extensions.is_empty() {
        bail!("package tar ends with extensions that describe no member");
    }

    // Exhaust the gzip stream before producing any promotable artifact. This
    // validates the trailer/CRC and rejects non-zero bytes after the tar EOF.
    let mut decoder = archive.into_inner();
    let mut trailing = [0u8; 8192];
    loop {
        let count = decoder
            .read(&mut trailing)
            .context("finish package gzip stream")?;
        if count == 0 {
            break;
        }
        if trailing[..count].iter().any(|byte| *byte != 0) {
            bail!("package TGZ contains non-zero data after the tar end marker");
        }
    }
    let cursor = decoder.into_inner().into_inner();
    if cursor.position() != bytes.len() as u64 {
        bail!("package TGZ contains trailing compressed data");
    }

    let dependencies =
        dependencies.ok_or_else(|| anyhow!("package {label} has no top-level package.json"))?;
    let index = derived_index::DerivedIndex {
        version: derived_index::DERIVED_INDEX_FORMAT_VERSION,
        files: derived.into_values().collect(),
    };
    let sidecar_bound = metadata_bytes
        .checked_mul(6)
        .and_then(|value| value.checked_add(1024))
        .ok_or_else(|| anyhow!("derived-index serialization bound overflow"))?;
    require_browser_working_set_budget(
        input_bytes,
        metadata_bytes,
        spool_bytes,
        0,
        sidecar_bound,
        0,
        limits.working_set_bytes,
    )?;
    let sidecar = derived_index::to_bytes(&index);
    drop(index);
    let sidecar_compression_bound = temporary_deflate_capacity_bound(sidecar.len() as u64)?;
    require_browser_working_set_budget(
        input_bytes,
        metadata_bytes,
        spool_bytes,
        0,
        sidecar.capacity() as u64,
        sidecar_compression_bound,
        limits.working_set_bytes,
    )?;
    let sidecar_compressed =
        compress_temporary_member(&sidecar).context("compress derived-index sidecar")?;
    spool_bytes = spool_bytes
        .checked_add(sidecar_compressed.capacity() as u64)
        .ok_or_else(|| anyhow!("package bundle temporary spool byte count overflow"))?;
    require_browser_working_set_budget(
        input_bytes,
        metadata_bytes,
        spool_bytes,
        0,
        sidecar.capacity() as u64,
        0,
        limits.working_set_bytes,
    )?;
    spooled.insert(
        derived_index::SIDECAR_NAME.to_string(),
        SpooledMember {
            identity: MemberIdentity {
                name: derived_index::SIDECAR_NAME.to_string(),
                raw_len: sidecar.len() as u64,
                raw_sha256: Sha256::digest(&sidecar).into(),
            },
            compressed: sidecar_compressed,
        },
    );
    drop(sidecar);
    let prepared_members = spooled.len() as u64;
    let identities = spooled
        .values()
        .map(|member| member.identity.clone())
        .collect::<Vec<_>>();
    let canonical_raw_bytes = identities.iter().try_fold(0u64, |total, member| {
        total
            .checked_add(member.raw_len)
            .ok_or_else(|| anyhow!("canonical package raw byte count overflow"))
    })?;
    let remaining_spool_bytes = std::cell::Cell::new(spool_bytes);
    let (key, artifact) = encode_from_member_bodies(
        label,
        &dependencies,
        identities,
        None,
        |output, raw, compressed| {
            require_browser_working_set_budget(
                input_bytes,
                metadata_bytes,
                remaining_spool_bytes.get(),
                output as u64,
                raw as u64,
                compressed as u64,
                limits.working_set_bytes,
            )?;
            Ok(())
        },
        |member, output| {
            let source = spooled
                .remove(&member.name)
                .ok_or_else(|| anyhow!("streamed member {} disappeared", member.name))?;
            let mut decoder = DeflateDecoder::new(source.compressed.as_slice());
            decoder
                .read_to_end(output)
                .with_context(|| format!("replay streamed member {}", member.name))?;
            remaining_spool_bytes.set(
                remaining_spool_bytes
                    .get()
                    .checked_sub(source.compressed.capacity() as u64)
                    .ok_or_else(|| anyhow!("streamed spool byte accounting underflow"))?,
            );
            Ok(())
        },
    )?;
    Ok(PreparedTgzArtifact {
        key,
        artifact,
        members: prepared_members,
        logical_raw_bytes,
        canonical_raw_bytes,
    })
}

fn require_browser_working_set_budget(
    input_bytes: u64,
    metadata_bytes: u64,
    spool_bytes: u64,
    output_bytes: u64,
    raw_chunk_bytes: u64,
    compressed_chunk_bytes: u64,
    maximum_working_set_bytes: u64,
) -> anyhow::Result<u64> {
    let working_set = input_bytes
        .checked_mul(2) // Worker ArrayBuffer plus wasm-bindgen's WASM copy.
        .and_then(|value| value.checked_add(metadata_bytes))
        .and_then(|value| value.checked_add(spool_bytes))
        .and_then(|value| value.checked_add(output_bytes))
        .and_then(|value| value.checked_add(raw_chunk_bytes))
        .and_then(|value| value.checked_add(compressed_chunk_bytes))
        .ok_or_else(|| {
            resource_policy_error("browser preparation working-set estimate overflowed")
        })?;
    if working_set > maximum_working_set_bytes {
        return Err(resource_policy_error(format!(
            "browser preparation per-call working-set policy is estimated at {} bytes; this carrier's preparation is estimated to require at least {working_set} bytes",
            maximum_working_set_bytes
        )));
    }
    Ok(working_set)
}

fn temporary_deflate_capacity_bound(raw_bytes: u64) -> anyhow::Result<u64> {
    // zlib's conservative compressBound formula, doubled to cover Vec growth
    // slack from the streaming encoder without relying on a post-allocation
    // measurement as the guard.
    raw_bytes
        .checked_add((raw_bytes + 7) >> 3)
        .and_then(|value| value.checked_add((raw_bytes + 63) >> 6))
        .and_then(|value| value.checked_add(64))
        .and_then(|value| value.checked_mul(2))
        .ok_or_else(|| anyhow!("temporary deflate capacity bound overflow"))
}

fn compress_temporary_member(body: &[u8]) -> io::Result<Vec<u8>> {
    // Unlike miniz_oxide::compress_to_vec, this encoder grows with emitted
    // compressed bytes instead of reserving roughly half the raw member up
    // front. That distinction is essential for highly compressible packages.
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(body)?;
    encoder.finish()
}

fn read_bounded_tar_entry(entry: &mut impl Read, bytes: u64) -> anyhow::Result<Vec<u8>> {
    let length = usize::try_from(bytes)
        .map_err(|_| anyhow!("package tar extension is too large for this host"))?;
    let mut body = Vec::new();
    body.try_reserve_exact(length)
        .context("reserve package tar extension")?;
    entry.read_to_end(&mut body)?;
    if body.len() != length {
        bail!(
            "package tar extension declared {length} bytes but yielded {}",
            body.len()
        );
    }
    Ok(body)
}

fn drain_tar_entry(entry: &mut impl Read) -> io::Result<()> {
    let mut buffer = [0u8; 8192];
    while entry.read(&mut buffer)? != 0 {}
    Ok(())
}

fn trim_gnu_long_name(mut body: Vec<u8>) -> Vec<u8> {
    if body.last() == Some(&0) {
        body.pop();
    }
    body
}

fn parse_local_pax(body: &[u8]) -> anyhow::Result<(Option<Vec<u8>>, Option<u64>)> {
    let mut path = None;
    let mut size = None;
    for candidate in tar::PaxExtensions::new(body) {
        let field = candidate.context("parse local PAX extension")?;
        if field.key_bytes() == b"path" && path.is_none() {
            path = Some(field.value_bytes().to_vec());
        } else if field.key_bytes() == b"size" && size.is_none() {
            let value = std::str::from_utf8(field.value_bytes())
                .context("PAX size is not UTF-8")?
                .parse::<u64>()
                .context("PAX size is not an unsigned integer")?;
            size = Some(value);
        }
    }
    Ok((path, size))
}

fn is_derived_index_resource_candidate(name: &str) -> bool {
    !name.contains('/')
        && !name.starts_with('.')
        && name.to_ascii_lowercase().ends_with(".json")
        && name != "package.json"
}

fn normalized_tgz_member_name(raw_name: &str) -> anyhow::Result<String> {
    let raw_name = raw_name.trim_start_matches("./");
    let name = raw_name.strip_prefix("package/").unwrap_or(raw_name);
    material::validate_member_name(name)?;
    Ok(name.to_string())
}

fn read_package_tgz_with_limits(
    bytes: &[u8],
    limits: PackageTgzLimits,
) -> anyhow::Result<BTreeMap<String, Vec<u8>>> {
    let decoder = GzDecoder::new(Cursor::new(bytes));
    let mut archive = tar::Archive::new(decoder);
    let mut result = BTreeMap::new();
    let mut member_count = 0u64;
    let mut expanded_bytes = 0u64;
    for candidate in archive.entries().context("read gzip/tar entries")? {
        let mut entry = candidate.context("read tar entry")?;
        member_count = member_count
            .checked_add(1)
            .ok_or_else(|| anyhow!("package bundle member count overflow"))?;
        if member_count > limits.members {
            bail!("package bundle has more than {} members", limits.members);
        }
        let member_bytes = entry.size();
        if member_bytes > limits.member_bytes {
            bail!(
                "package bundle member is {member_bytes} bytes; limit is {} bytes",
                limits.member_bytes
            );
        }
        expanded_bytes = expanded_bytes
            .checked_add(member_bytes)
            .ok_or_else(|| anyhow!("package bundle expanded byte count overflow"))?;
        if expanded_bytes > limits.expanded_bytes {
            bail!(
                "package bundle expands past {} bytes",
                limits.expanded_bytes
            );
        }
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let path = entry.path().context("read tar member path")?;
        let raw_name = path
            .to_str()
            .ok_or_else(|| anyhow!("package bundle member name is not UTF-8"))?
            .to_string();
        let name = normalized_tgz_member_name(&raw_name)
            .map_err(|_| anyhow!("unsafe package bundle member path: {raw_name:?}"))?;
        let member_len = usize::try_from(member_bytes)
            .map_err(|_| anyhow!("package bundle member is too large for this host"))?;
        let mut body = Vec::new();
        body.try_reserve_exact(member_len)
            .with_context(|| format!("reserve package bundle member {name}"))?;
        entry
            .read_to_end(&mut body)
            .with_context(|| format!("read package bundle member {name}"))?;
        if body.len() != member_len {
            bail!(
                "package bundle member {name} declared {member_len} bytes but yielded {}",
                body.len()
            );
        }
        if result.insert(name.clone(), body).is_some() {
            bail!("duplicate package bundle member after root normalization: {name}");
        }
    }
    Ok(result)
}

/// One entry in a [`BundleManifest`]: a pinned package and the bundle blob that
/// carries it. `bundle` is the name/URL of the blob the browser fetches (the
/// builder writes `<id>#<ver>.tgz`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleManifestEntry {
    /// Package id (`hl7.fhir.r4.core`).
    pub id: String,
    /// Exact pinned version (`4.0.1`).
    pub version: String,
    /// The bundle blob's name (relative to the manifest), e.g.
    /// `hl7.fhir.r4.core#4.0.1.tgz`.
    pub bundle: String,
    /// SHA-256 (hex) of the bundle blob, so a reader can verify integrity.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub sha256: Option<String>,
}

/// The editor's "lockfile": the pinned set of package bundles a project needs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BundleManifest {
    #[serde(rename = "bundle-format-version")]
    pub format_version: u32,
    pub packages: Vec<BundleManifestEntry>,
}

impl BundleManifest {
    /// A fresh manifest at the current [`BUNDLE_FORMAT_VERSION`].
    pub fn new() -> Self {
        Self {
            format_version: BUNDLE_FORMAT_VERSION,
            packages: Vec::new(),
        }
    }

    /// Parse a manifest, rejecting a version mismatch (treated as absent so the
    /// caller re-fetches / rebuilds).
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let m: BundleManifest = serde_json::from_slice(bytes).ok()?;
        if m.format_version != BUNDLE_FORMAT_VERSION {
            return None;
        }
        Some(m)
    }

    /// Serialize to pretty JSON bytes (the manifest is small and human-inspected).
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec_pretty(self).expect("BundleManifest serializes")
    }
}

impl Default for BundleManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// A read-only in-memory [`PackageSource`]. Holds every mounted file's bytes keyed
/// by its full path under a synthetic cache root, plus the set of directories that
/// exist (so `is_dir`/`read_dir` answer faithfully without a real FS).
#[derive(Debug, Clone)]
pub struct BundleSource {
    /// Synthetic cache root the mounted packages hang under. All the
    /// `<cache>/<id>#<ver>/package/...` paths the caller builds must join this
    /// root, which is exactly what happens when the caller passes
    /// [`BundleSource::cache_root`] as the `cache_dir` to `PackageContext::new_with`
    /// / `PackageStore::for_project_with`.
    root: PathBuf,
    /// Immutable per-package mount layers. Cloning a BundleSource clones only
    /// this small label map and its `Rc`s, never previously mounted file bytes.
    layers: BTreeMap<String, Rc<BundleLayer>>,
}

/// Runtime footprint and lazy-inflate counters for all compact prepared layers.
/// Backings shared by several package layers are counted exactly once.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "wire-contract", derive(ts_rs::TS))]
#[cfg_attr(feature = "wire-contract", derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct BundleCompressionMetrics {
    pub compressed_retained_bytes: u64,
    pub declared_raw_bytes: u64,
    pub chunks_inflated: u64,
    pub raw_inflated_bytes: u64,
    pub cache_hits: u64,
    pub cached_raw_bytes: u64,
}

#[derive(Debug)]
struct BundleLayer {
    files: LayerFiles,
    /// Directory paths introduced by those files.
    dirs: Rc<std::collections::BTreeSet<PathBuf>>,
}

#[derive(Debug)]
enum LayerFiles {
    /// Cold/native package material already exists as independently owned files.
    Owned(BTreeMap<PathBuf, Vec<u8>>),
    /// Warm prepared material remains one compact immutable artifact/batch;
    /// its member directory is mounted immediately and chunks inflate lazily.
    Compressed {
        files: PreparedCompressedFiles,
        names: Rc<BTreeMap<PathBuf, String>>,
    },
}

impl BundleLayer {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        match &self.files {
            LayerFiles::Owned(files) => files.get(path).cloned().ok_or_else(not_found),
            LayerFiles::Compressed { files, names } => names
                .get(path)
                .ok_or_else(not_found)
                .and_then(|name| files.read(name)),
        }
    }

    fn contains(&self, path: &Path) -> bool {
        match &self.files {
            LayerFiles::Owned(files) => files.contains_key(path),
            LayerFiles::Compressed { names, .. } => names.contains_key(path),
        }
    }

    fn paths(&self) -> Box<dyn Iterator<Item = &PathBuf> + '_> {
        match &self.files {
            LayerFiles::Owned(files) => Box::new(files.keys()),
            LayerFiles::Compressed { names, .. } => Box::new(names.keys()),
        }
    }

    fn append_file_children(
        &self,
        path: &Path,
        seen: &mut std::collections::BTreeSet<String>,
        out: &mut Vec<DirEntry>,
    ) {
        for fpath in self.paths() {
            if fpath.parent() != Some(path) {
                continue;
            }
            if let Some(name) = fpath.file_name().and_then(|name| name.to_str()) {
                if seen.insert(name.to_string()) {
                    out.push(DirEntry {
                        file_name: name.to_string(),
                        is_file: true,
                    });
                }
            }
        }
    }

    fn append_directory_children(
        &self,
        path: &Path,
        seen: &mut std::collections::BTreeSet<String>,
        out: &mut Vec<DirEntry>,
    ) {
        for dpath in self.dirs.iter() {
            if dpath.parent() != Some(path) {
                continue;
            }
            if let Some(name) = dpath.file_name().and_then(|name| name.to_str()) {
                if seen.insert(name.to_string()) {
                    out.push(DirEntry {
                        file_name: name.to_string(),
                        is_file: false,
                    });
                }
            }
        }
    }
}

fn not_found() -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, "no such file in bundle")
}

impl BundleSource {
    /// Create an empty bundle source rooted at a synthetic in-memory cache dir.
    /// The root is a stable virtual path (never touched on disk) that callers pass
    /// as the `cache_dir`. Get it back via [`BundleSource::cache_root`].
    pub fn new() -> Self {
        let root = PathBuf::from("/__bundle_cache__");
        Self {
            root,
            layers: BTreeMap::new(),
        }
    }

    /// The synthetic cache root to pass as `cache_dir` to the store/context
    /// constructors. Package dirs live at `<root>/<id>#<ver>/package`.
    pub fn cache_root(&self) -> &Path {
        &self.root
    }

    /// Share immutable mounted carrier bytes and decoded directory/member
    /// indexes while replacing every mutable decompression cache. Owned layers
    /// have no read cache and can be shared as-is. Prepared ranges which shared
    /// one batch cache in the source share one new cache in the fork.
    pub fn fork_read_cache(&self) -> Self {
        let mut backings = BTreeMap::new();
        let layers = self
            .layers
            .iter()
            .map(|(label, layer)| {
                let forked = match &layer.files {
                    LayerFiles::Owned(_) => Rc::clone(layer),
                    LayerFiles::Compressed { files, names } => Rc::new(BundleLayer {
                        files: LayerFiles::Compressed {
                            files: files.fork_read_cache(&mut backings),
                            names: Rc::clone(names),
                        },
                        dirs: Rc::clone(&layer.dirs),
                    }),
                };
                (label.clone(), forked)
            })
            .collect();
        Self {
            root: self.root.clone(),
            layers,
        }
    }

    /// Snapshot lazy prepared-package storage counters without reading a body.
    pub fn compression_metrics(&self) -> BundleCompressionMetrics {
        let mut result = BundleCompressionMetrics::default();
        let mut backings = std::collections::BTreeSet::new();
        for layer in self.layers.values() {
            let LayerFiles::Compressed { files, .. } = &layer.files else {
                continue;
            };
            result.declared_raw_bytes = result
                .declared_raw_bytes
                .saturating_add(files.declared_raw_bytes());
            if backings.insert(files.backing_identity()) {
                let metrics = files.backing_metrics();
                result.compressed_retained_bytes = result
                    .compressed_retained_bytes
                    .saturating_add(metrics.compressed_retained_bytes);
                result.chunks_inflated = result
                    .chunks_inflated
                    .saturating_add(metrics.chunks_inflated);
                result.raw_inflated_bytes = result
                    .raw_inflated_bytes
                    .saturating_add(metrics.raw_inflated_bytes);
                result.cache_hits = result.cache_hits.saturating_add(metrics.cache_hits);
                result.cached_raw_bytes = result
                    .cached_raw_bytes
                    .saturating_add(metrics.cached_raw_bytes);
            }
        }
        result
    }

    /// Mount one package's already-inflated `file_name -> bytes` entries under
    /// `<root>/<id>#<ver>/package/`. `label` is the `<id>#<ver>` directory name.
    /// The entries are the package-relative file names from the bundle (resource
    /// JSONs, `.index.json`, `.derived-index.json`, `package.json`).
    pub fn mount_package<I, S>(&mut self, label: &str, entries: I)
    where
        I: IntoIterator<Item = (S, Vec<u8>)>,
        S: AsRef<str>,
    {
        let package_dir = self.root.join(label).join("package");
        let mut files = BTreeMap::new();
        let mut dirs = std::collections::BTreeSet::new();
        Self::add_dir(&self.root, &mut dirs, &package_dir);
        for (name, bytes) in entries {
            let path = package_dir.join(name.as_ref());
            if let Some(parent) = path.parent() {
                Self::add_dir(&self.root, &mut dirs, parent);
            }
            files.insert(path, bytes);
        }
        self.layers.insert(
            label.to_string(),
            Rc::new(BundleLayer {
                files: LayerFiles::Owned(files),
                dirs: Rc::new(dirs),
            }),
        );
    }

    /// Mount a validated compact package directory. No compressed chunk is
    /// inflated until `PackageSource::read` asks for one of its members.
    pub(crate) fn mount_prepared_compressed(
        &mut self,
        label: &str,
        files: PreparedCompressedFiles,
    ) {
        let package_dir = self.root.join(label).join("package");
        let mut names = BTreeMap::new();
        let mut dirs = std::collections::BTreeSet::new();
        Self::add_dir(&self.root, &mut dirs, &package_dir);
        for name in files.names() {
            let path = package_dir.join(name);
            if let Some(parent) = path.parent() {
                Self::add_dir(&self.root, &mut dirs, parent);
            }
            names.insert(path, name.clone());
        }
        self.layers.insert(
            label.to_string(),
            Rc::new(BundleLayer {
                files: LayerFiles::Compressed {
                    files,
                    names: Rc::new(names),
                },
                dirs: Rc::new(dirs),
            }),
        );
    }

    /// Record `dir` and all its ancestors up to (and including) the root as
    /// existing directories.
    fn add_dir(root: &Path, dirs: &mut std::collections::BTreeSet<PathBuf>, dir: &Path) {
        let mut cur = Some(dir);
        while let Some(d) = cur {
            dirs.insert(d.to_path_buf());
            if d == root {
                break;
            }
            cur = d.parent();
        }
    }

    fn layer_for_path(&self, path: &Path) -> Option<&BundleLayer> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let label = relative.components().next()?.as_os_str().to_str()?;
        self.layers.get(label).map(Rc::as_ref)
    }
}

impl PackageSource for BundleSource {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.layer_for_path(path).ok_or_else(not_found)?.read(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<DirEntry>> {
        if !self.is_dir(path) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no such directory in bundle",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        if path == self.root {
            // The root is the only directory whose direct children can come
            // from several mounted package layers. Preserve the historical
            // files-before-directories order across that complete union.
            for layer in self.layers.values() {
                layer.append_file_children(path, &mut seen, &mut out);
            }
            for layer in self.layers.values() {
                layer.append_directory_children(path, &mut seen, &mut out);
            }
        } else {
            // Every non-root path is owned by its first component below root,
            // exactly like read/exists/is_dir. Avoid scanning unrelated package
            // layers for each package-specific directory listing.
            let layer = self
                .layer_for_path(path)
                .expect("is_dir already proved a mounted bundle layer");
            layer.append_file_children(path, &mut seen, &mut out);
            layer.append_directory_children(path, &mut seen, &mut out);
        }
        Ok(out)
    }

    fn exists(&self, path: &Path) -> bool {
        self.is_dir(path)
            || self
                .layer_for_path(path)
                .is_some_and(|layer| layer.contains(path))
    }

    fn is_file(&self, path: &Path) -> bool {
        self.layer_for_path(path)
            .is_some_and(|layer| layer.contains(path))
    }

    fn is_dir(&self, path: &Path) -> bool {
        path == self.root
            || self
                .layer_for_path(path)
                .is_some_and(|layer| layer.dirs.contains(path))
    }

    fn fork_read_cache(&self) -> io::Result<Box<dyn PackageSource>> {
        Ok(Box::new(BundleSource::fork_read_cache(self)))
    }

    // write_new: default (read-only) — the sidecar write-once fails soft, and the
    // bundle already ships `.derived-index.json`, so `derived_index::load` reads it
    // straight from the mounted files and never needs to write.
}

impl Default for BundleSource {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{write::GzEncoder, Compression};
    use std::io::Write;
    use tar::{Builder, EntryType, Header};

    fn entry_shape(entries: Vec<DirEntry>) -> Vec<(String, bool)> {
        entries
            .into_iter()
            .map(|entry| (entry.file_name, entry.is_file))
            .collect()
    }

    fn legacy_read_dir(source: &BundleSource, path: &Path) -> io::Result<Vec<DirEntry>> {
        if !source.is_dir(path) {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no such directory in bundle",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut out = Vec::new();
        for layer in source.layers.values() {
            layer.append_file_children(path, &mut seen, &mut out);
        }
        for layer in source.layers.values() {
            layer.append_directory_children(path, &mut seen, &mut out);
        }
        Ok(out)
    }

    fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut archive = Builder::new(encoder);
        for (path, bytes) in files {
            let mut header = Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            archive
                .append_data(&mut header, path, Cursor::new(*bytes))
                .unwrap();
        }
        archive.finish().unwrap();
        archive.into_inner().unwrap().finish().unwrap()
    }

    fn raw_tgz(entries: &[(&str, EntryType, &[u8])]) -> Vec<u8> {
        let encoder = GzEncoder::new(Vec::new(), Compression::default());
        let mut archive = Builder::new(encoder);
        for (path, entry_type, bytes) in entries {
            let mut header = Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_entry_type(*entry_type);
            header.set_cksum();
            archive
                .append_data(&mut header, path, Cursor::new(*bytes))
                .unwrap();
        }
        archive.finish().unwrap();
        archive.into_inner().unwrap().finish().unwrap()
    }

    fn pax_record(key: &str, value: &str) -> Vec<u8> {
        let payload = format!(" {key}={value}\n");
        let mut length = payload.len() + 1;
        loop {
            let next = payload.len() + length.to_string().len();
            if next == length {
                return format!("{length}{payload}").into_bytes();
            }
            length = next;
        }
    }

    fn gunzip(bytes: &[u8]) -> Vec<u8> {
        let mut decoded = Vec::new();
        GzDecoder::new(bytes).read_to_end(&mut decoded).unwrap();
        decoded
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn tgz_with_declared_member(path: &str, declared_bytes: u64) -> Vec<u8> {
        let mut header = Header::new_gnu();
        header.set_path(path).unwrap();
        header.set_size(declared_bytes);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        let mut tar = Vec::from(header.as_bytes());
        tar.extend_from_slice(&[0; 1024]);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&tar).unwrap();
        encoder.finish().unwrap()
    }

    fn package_json() -> &'static [u8] {
        br#"{"name":"example.pkg","version":"1.2.3","dependencies":{"dep.pkg":"2.0.0"}}"#
    }

    #[test]
    fn streaming_tgz_is_byte_identical_to_map_preparation_in_any_tar_order() {
        let first = tgz(&[
            ("package/nested/template.txt", b"nested"),
            (
                "package/StructureDefinition-demo.json",
                br#"{"resourceType":"StructureDefinition","id":"demo","url":"http://example.test/StructureDefinition/demo","name":"Demo","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Patient"}"#,
            ),
            (
                "package/.derived-index.json",
                br#"{"derived-index-version":999,"files":[{"filename":"stale"}]}"#,
            ),
            ("package/package.json", package_json()),
            ("package/UPPER.JSON", br#"{"resourceType":"Patient","id":"upper"}"#),
            ("package/Broken.json", b"not-json"),
        ]);
        let second = tgz(&[
            ("package/Broken.json", b"not-json"),
            ("package/UPPER.JSON", br#"{"resourceType":"Patient","id":"upper"}"#),
            ("package/package.json", package_json()),
            (
                "package/.derived-index.json",
                br#"{"derived-index-version":999,"files":[{"filename":"different-stale"}]}"#,
            ),
            (
                "package/StructureDefinition-demo.json",
                br#"{"resourceType":"StructureDefinition","id":"demo","url":"http://example.test/StructureDefinition/demo","name":"Demo","baseDefinition":"http://hl7.org/fhir/StructureDefinition/Patient"}"#,
            ),
            ("package/nested/template.txt", b"nested"),
        ]);
        let expected_entries = read_package_tgz(&first).unwrap();
        let expected =
            crate::PreparedPackage::prepare("example.pkg#1.2.3", expected_entries).unwrap();
        let expected_bytes = expected.encode();
        // Frozen from the unmodified 652581b8 v3 encoder in a detached build.
        // This catches accidental format drift even if both current preparation
        // paths share a changed encoder implementation.
        assert_eq!(expected_bytes.len(), 1077);
        assert_eq!(
            hex::encode(Sha256::digest(&expected_bytes)),
            "f5054d7b4edc8dda750be803cd8ca6fa7e4352859087f31ec7009b60aa542118"
        );
        let streamed_first = prepare_package_tgz_streaming("example.pkg#1.2.3", &first).unwrap();
        let streamed_second = prepare_package_tgz_streaming("example.pkg#1.2.3", &second).unwrap();

        assert_eq!(streamed_first.key, expected.key);
        assert_eq!(streamed_first.artifact, expected_bytes);
        assert_eq!(streamed_second.artifact, expected_bytes);
        assert_eq!(streamed_first.artifact, streamed_second.artifact);

        let decoded =
            crate::PreparedPackage::decode_expected(&streamed_first.artifact, &streamed_first.key)
                .unwrap();
        assert_eq!(
            decoded.files.materialize_all().unwrap(),
            expected.files.materialize_all().unwrap()
        );
    }

    #[test]
    fn streaming_tgz_crosses_a_legacy_aggregate_limit_with_bounded_raw_state() {
        let first = vec![b'a'; 700];
        let second = vec![b'b'; 700];
        let carrier = tgz(&[
            ("package/package.json", package_json()),
            ("package/nested/first.bin", &first),
            ("package/nested/second.bin", &second),
        ]);
        let old_error = read_package_tgz_with_limits(
            &carrier,
            PackageTgzLimits {
                members: 8,
                member_bytes: 1024,
                expanded_bytes: 1024,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(old_error.contains("expands past 1024 bytes"), "{old_error}");

        let streamed = prepare_package_tgz_streaming_with_limits(
            "example.pkg#1.2.3",
            &carrier,
            StreamingTgzLimits {
                logical_bytes: 16 * 1024,
                working_set_bytes: 1024 * 1024,
                metadata_bytes: 64 * 1024,
                extension_records: 16,
                extension_bytes: 64 * 1024,
                expansion_ratio: 128,
                ratio_allowance_bytes: 4096,
            },
        )
        .unwrap();
        assert!(streamed.logical_raw_bytes > 1024);
    }

    #[test]
    fn streaming_tgz_validates_gzip_completion_and_rejects_extra_members() {
        let valid = tgz(&[("package/package.json", package_json())]);

        let mut bad_crc = valid.clone();
        let crc_byte = bad_crc.len() - 8;
        bad_crc[crc_byte] ^= 0x80;
        let crc_error = prepare_package_tgz_streaming("example.pkg#1.2.3", &bad_crc).unwrap_err();
        assert!(!is_package_resource_policy_exhaustion(&crc_error));

        let truncated = &valid[..valid.len() - 4];
        assert!(
            prepare_package_tgz_streaming("example.pkg#1.2.3", truncated).is_err(),
            "a truncated gzip trailer must fail"
        );

        let mut concatenated = valid.clone();
        concatenated.extend_from_slice(&gzip(b"second gzip member"));
        let error = prepare_package_tgz_streaming("example.pkg#1.2.3", &concatenated)
            .unwrap_err()
            .to_string();
        assert!(error.contains("trailing compressed data"), "{error}");

        let mut raw_tar = gunzip(&valid);
        raw_tar.extend_from_slice(b"not tar padding");
        let error = prepare_package_tgz_streaming("example.pkg#1.2.3", &gzip(&raw_tar))
            .unwrap_err()
            .to_string();
        assert!(error.contains("non-zero data after the tar end"), "{error}");
    }

    #[test]
    fn streaming_tgz_applies_bounded_gnu_and_pax_paths() {
        let long_path = format!("package/nested/{}.txt", "long".repeat(40));
        let mut long_name = long_path.as_bytes().to_vec();
        long_name.push(0);
        let carrier = raw_tgz(&[
            ("package/package.json", EntryType::Regular, package_json()),
            ("././@LongLink", EntryType::GNULongName, &long_name),
            ("placeholder", EntryType::Regular, b"gnu body"),
        ]);
        let streamed = prepare_package_tgz_streaming("example.pkg#1.2.3", &carrier).unwrap();
        let decoded =
            crate::PreparedPackage::decode_expected(&streamed.artifact, &streamed.key).unwrap();
        assert_eq!(
            decoded
                .files
                .read(long_path.strip_prefix("package/").unwrap())
                .unwrap(),
            Some(b"gnu body".to_vec())
        );

        let pax_path = "package/nested/from-pax.txt";
        let pax = pax_record("path", pax_path);
        let carrier = raw_tgz(&[
            ("package/package.json", EntryType::Regular, package_json()),
            ("PaxHeader", EntryType::XHeader, &pax),
            ("placeholder", EntryType::Regular, b"pax body"),
        ]);
        let streamed = prepare_package_tgz_streaming("example.pkg#1.2.3", &carrier).unwrap();
        let decoded =
            crate::PreparedPackage::decode_expected(&streamed.artifact, &streamed.key).unwrap();
        assert_eq!(
            decoded.files.read("nested/from-pax.txt").unwrap(),
            Some(b"pax body".to_vec())
        );
    }

    #[test]
    fn streaming_tgz_rejects_oversized_and_excess_extension_records_before_retention() {
        let oversized = vec![b'x'; 65];
        let carrier = raw_tgz(&[
            ("package/package.json", EntryType::Regular, package_json()),
            ("././@LongLink", EntryType::GNULongName, &oversized),
            ("placeholder", EntryType::Regular, b"body"),
        ]);
        let mut limits = STREAMING_TGZ_LIMITS;
        limits.logical_bytes = 64 * 1024;
        limits.working_set_bytes = 1024 * 1024;
        limits.metadata_bytes = 64 * 1024;
        limits.extension_bytes = 64;
        limits.expansion_ratio = 1024;
        limits.ratio_allowance_bytes = 64 * 1024;
        let error =
            prepare_package_tgz_streaming_with_limits("example.pkg#1.2.3", &carrier, limits)
                .unwrap_err();
        assert!(is_package_resource_policy_exhaustion(&error), "{error:#}");
        assert!(
            error.to_string().contains("record declares 65 bytes"),
            "{error}"
        );

        let carrier = raw_tgz(&[
            ("GlobalOne", EntryType::XGlobalHeader, b"one"),
            ("GlobalTwo", EntryType::XGlobalHeader, b"two"),
            ("package/package.json", EntryType::Regular, package_json()),
        ]);
        limits.extension_bytes = 64;
        limits.extension_records = 1;
        let error =
            prepare_package_tgz_streaming_with_limits("example.pkg#1.2.3", &carrier, limits)
                .unwrap_err();
        assert!(is_package_resource_policy_exhaustion(&error));
        assert!(
            error
                .to_string()
                .contains("at most 1 tar extension records"),
            "{error}"
        );
    }

    #[test]
    fn streaming_tgz_counts_zero_padding_after_tar_eof_without_retaining_it() {
        let valid = tgz(&[("package/package.json", package_json())]);
        let mut raw_tar = gunzip(&valid);
        let limit = raw_tar.len() as u64 + 64;
        raw_tar.extend(std::iter::repeat_n(0, 4096));
        let carrier = gzip(&raw_tar);
        let error = prepare_package_tgz_streaming_with_limits(
            "example.pkg#1.2.3",
            &carrier,
            StreamingTgzLimits {
                logical_bytes: limit,
                working_set_bytes: 1024 * 1024,
                metadata_bytes: 64 * 1024,
                extension_records: 16,
                extension_bytes: 64 * 1024,
                expansion_ratio: 1024,
                ratio_allowance_bytes: limit,
            },
        )
        .unwrap_err();
        assert!(is_package_resource_policy_exhaustion(&error), "{error:#}");
        let error = format!("{error:#}");
        assert!(error.contains("resource policy allows"), "{error}");
    }

    #[test]
    fn streaming_tgz_caps_package_manifest_before_value_parsing() {
        let mut manifest = package_json().to_vec();
        manifest.extend(std::iter::repeat_n(b' ', 1024));
        let carrier = tgz(&[("package/package.json", &manifest)]);
        let error = prepare_package_tgz_streaming_with_limits(
            "example.pkg#1.2.3",
            &carrier,
            StreamingTgzLimits {
                logical_bytes: 16 * 1024,
                working_set_bytes: 1024 * 1024,
                metadata_bytes: 512,
                extension_records: 16,
                extension_bytes: 256,
                expansion_ratio: 128,
                ratio_allowance_bytes: 16 * 1024,
            },
        )
        .unwrap_err();
        assert!(is_package_resource_policy_exhaustion(&error));
        assert!(
            error.to_string().contains("retained package metadata"),
            "{error}"
        );
    }

    #[test]
    fn tgz_reader_normalizes_registry_and_baked_roots_once() {
        let bytes = tgz(&[
            ("package/package.json", b"registry-root"),
            ("package/nested/template.txt", b"nested"),
            ("baked.txt", b"baked-root"),
        ]);
        let entries = read_package_tgz(&bytes).unwrap();
        assert_eq!(entries["package.json"], b"registry-root");
        assert_eq!(entries["nested/template.txt"], b"nested");
        assert_eq!(entries["baked.txt"], b"baked-root");
    }

    #[test]
    fn tgz_reader_rejects_malformed_and_duplicate_normalized_carriers() {
        assert!(read_package_tgz(b"not gzip").is_err());
        let duplicate = tgz(&[("package/same.txt", b"first"), ("same.txt", b"second")]);
        assert!(read_package_tgz(&duplicate)
            .unwrap_err()
            .to_string()
            .contains("duplicate package bundle member"));
    }

    #[test]
    fn tgz_reader_enforces_member_count_and_expanded_byte_limits() {
        let exact = tgz(&[("first", b"12"), ("second", b"345")]);
        let exact_limits = PackageTgzLimits {
            members: 2,
            member_bytes: 3,
            expanded_bytes: 5,
        };
        assert_eq!(
            read_package_tgz_with_limits(&exact, exact_limits)
                .unwrap()
                .values()
                .map(Vec::len)
                .sum::<usize>(),
            5
        );

        let member_count_error = read_package_tgz_with_limits(
            &exact,
            PackageTgzLimits {
                members: 1,
                ..exact_limits
            },
        )
        .unwrap_err()
        .to_string();
        assert!(member_count_error.contains("more than 1 members"));

        let member_size_error = read_package_tgz_with_limits(
            &exact,
            PackageTgzLimits {
                member_bytes: 2,
                ..exact_limits
            },
        )
        .unwrap_err()
        .to_string();
        assert!(member_size_error.contains("limit is 2 bytes"));

        let total_size_error = read_package_tgz_with_limits(
            &exact,
            PackageTgzLimits {
                expanded_bytes: 4,
                ..exact_limits
            },
        )
        .unwrap_err()
        .to_string();
        assert!(total_size_error.contains("expands past 4 bytes"));
    }

    #[test]
    fn tgz_reader_rejects_oversized_declaration_before_reading_its_body() {
        let carrier = tgz_with_declared_member("package/huge.bin", 11);
        let error = read_package_tgz_with_limits(
            &carrier,
            PackageTgzLimits {
                members: 1,
                member_bytes: 10,
                expanded_bytes: 10,
            },
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("limit is 10 bytes"), "{error}");
    }

    #[test]
    fn manifest_roundtrips_and_rejects_wrong_version() {
        let mut m = BundleManifest::new();
        m.packages.push(BundleManifestEntry {
            id: "hl7.fhir.r4.core".into(),
            version: "4.0.1".into(),
            bundle: "hl7.fhir.r4.core#4.0.1.tgz".into(),
            sha256: Some("deadbeef".into()),
        });
        let bytes = m.to_bytes();
        assert_eq!(BundleManifest::parse(&bytes), Some(m));
        let bad = br#"{"bundle-format-version":999,"packages":[]}"#;
        assert!(BundleManifest::parse(bad).is_none());
    }

    #[test]
    fn mounts_and_serves_a_package_dir() {
        let mut src = BundleSource::new();
        src.mount_package(
            "hl7.fhir.r4.core#4.0.1",
            vec![
                (".index.json", br#"{"files":[]}"#.to_vec()),
                (
                    "StructureDefinition-Patient.json",
                    br#"{"resourceType":"StructureDefinition","id":"Patient"}"#.to_vec(),
                ),
            ],
        );
        let pkg = src
            .cache_root()
            .join("hl7.fhir.r4.core#4.0.1")
            .join("package");
        assert!(src.is_dir(&pkg));
        assert!(src.is_dir(src.cache_root()));
        assert!(src.exists(&pkg.join(".index.json")));
        assert_eq!(
            src.read(&pkg.join("StructureDefinition-Patient.json"))
                .unwrap(),
            br#"{"resourceType":"StructureDefinition","id":"Patient"}"#
        );
        let mut names: Vec<String> = src
            .read_dir(&pkg)
            .unwrap()
            .into_iter()
            .map(|e| e.file_name)
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![".index.json", "StructureDefinition-Patient.json"]
        );
        // The package dir shows up as a subdir of its `<id>#<ver>` parent.
        let ver_dir = src.cache_root().join("hl7.fhir.r4.core#4.0.1");
        let sub: Vec<String> = src
            .read_dir(&ver_dir)
            .unwrap()
            .into_iter()
            .map(|e| e.file_name)
            .collect();
        assert_eq!(sub, vec!["package"]);
        // read_dir of the root lists the version dir.
        let roots: Vec<String> = src
            .read_dir(src.cache_root())
            .unwrap()
            .into_iter()
            .map(|e| e.file_name)
            .collect();
        assert_eq!(roots, vec!["hl7.fhir.r4.core#4.0.1"]);
        // A read-only source: write_new is unsupported (fail-soft for sidecars).
        assert!(src
            .write_new(&pkg.join(".derived-index.json"), b"{}")
            .is_err());
    }

    #[test]
    fn read_dir_routes_non_root_paths_without_changing_legacy_results() {
        let empty = BundleSource::new();
        assert!(empty.read_dir(empty.cache_root()).unwrap().is_empty());

        let mut source = BundleSource::new();
        source.mount_package(
            "a#1",
            [
                ("alpha.json", b"alpha".to_vec()),
                ("nested/one.json", b"one".to_vec()),
                ("same", b"file wins".to_vec()),
                ("same/deep.json", b"hidden directory entry".to_vec()),
            ],
        );
        source.mount_package(
            "b#1",
            [
                ("beta.json", b"beta".to_vec()),
                ("nested/two.json", b"two".to_vec()),
            ],
        );

        let root = source.cache_root().to_path_buf();
        let a = root.join("a#1");
        let a_package = a.join("package");
        let a_nested = a_package.join("nested");
        let b = root.join("b#1");
        let b_package = b.join("package");
        let b_nested = b_package.join("nested");
        for path in [&root, &a, &a_package, &a_nested, &b, &b_package, &b_nested] {
            assert_eq!(
                entry_shape(source.read_dir(path).unwrap()),
                entry_shape(legacy_read_dir(&source, path).unwrap()),
                "optimized listing differs at {}",
                path.display()
            );
        }
        assert_eq!(
            entry_shape(source.read_dir(&root).unwrap()),
            vec![("a#1".into(), false), ("b#1".into(), false)]
        );
        assert_eq!(
            entry_shape(source.read_dir(&a_package).unwrap()),
            vec![
                ("alpha.json".into(), true),
                ("same".into(), true),
                ("nested".into(), false),
            ]
        );
        assert_eq!(
            entry_shape(source.read_dir(&b_package).unwrap()),
            vec![("beta.json".into(), true), ("nested".into(), false)]
        );
        for missing in [
            root.join("absent#1"),
            a_package.join("alpha.json"),
            a_package.join("absent"),
            PathBuf::from("/__outside_bundle__"),
        ] {
            assert_eq!(
                source.read_dir(&missing).unwrap_err().kind(),
                io::ErrorKind::NotFound
            );
        }
    }

    #[test]
    fn read_dir_preserves_clone_and_same_label_replacement_namespaces() {
        let mut source = BundleSource::new();
        source.mount_package("p#1", [("old/path.json", b"old".to_vec())]);
        let prior = source.clone();
        source.mount_package("p#1", [("new/path.json", b"new".to_vec())]);

        let prior_package = prior.cache_root().join("p#1/package");
        let current_package = source.cache_root().join("p#1/package");
        assert_eq!(
            entry_shape(prior.read_dir(&prior_package).unwrap()),
            vec![("old".into(), false)]
        );
        assert_eq!(
            entry_shape(source.read_dir(&current_package).unwrap()),
            vec![("new".into(), false)]
        );
        assert_eq!(
            entry_shape(source.read_dir(source.cache_root()).unwrap()),
            vec![("p#1".into(), false)]
        );
    }

    #[test]
    fn compressed_and_forked_read_dir_are_equivalent_and_non_inflating() {
        let prepared = crate::PreparedPackage::prepare(
            "p#1",
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"p","version":"1"}"#.to_vec(),
                ),
                ("first.txt".into(), b"first".to_vec()),
                ("nested/second.txt".into(), b"second".to_vec()),
            ]),
        )
        .unwrap();
        let decoded =
            crate::PreparedPackage::decode_expected(&prepared.encode(), &prepared.key).unwrap();
        let mut source = BundleSource::new();
        decoded.mount_into(&mut source);
        source.mount_package("owned#1", [("other.txt", b"other".to_vec())]);
        let forked = source.fork_read_cache();
        let before = source.compression_metrics();
        let forked_before = forked.compression_metrics();

        let paths = [
            source.cache_root().to_path_buf(),
            source.cache_root().join("p#1"),
            source.cache_root().join("p#1/package"),
            source.cache_root().join("p#1/package/nested"),
            source.cache_root().join("owned#1/package"),
        ];
        for path in &paths {
            let expected = entry_shape(legacy_read_dir(&source, path).unwrap());
            assert_eq!(entry_shape(source.read_dir(path).unwrap()), expected);
            assert_eq!(entry_shape(forked.read_dir(path).unwrap()), expected);
        }
        assert_eq!(source.compression_metrics(), before);
        assert_eq!(forked.compression_metrics(), forked_before);
    }

    #[test]
    fn clones_share_immutable_layers_and_diverge_by_appending() {
        let mut original = BundleSource::new();
        original.mount_package("a#1", [("a.txt", b"a".to_vec())]);
        let mut transaction = original.clone();
        assert!(Rc::ptr_eq(
            &original.layers["a#1"],
            &transaction.layers["a#1"]
        ));

        transaction.mount_package("b#1", [("b.txt", b"b".to_vec())]);
        assert_eq!(original.layers.len(), 1);
        assert_eq!(transaction.layers.len(), 2);
        assert!(original
            .read(&original.cache_root().join("b#1/package/b.txt"))
            .is_err());
        assert_eq!(
            transaction
                .read(&transaction.cache_root().join("a#1/package/a.txt"))
                .unwrap(),
            b"a"
        );
    }

    #[test]
    fn prepared_layer_retains_compressed_directory_and_reads_lazily() {
        let prepared = crate::PreparedPackage::prepare(
            "p#1",
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"p","version":"1"}"#.to_vec(),
                ),
                ("first.txt".into(), b"first".to_vec()),
                ("second.txt".into(), b"second".to_vec()),
            ]),
        )
        .unwrap();
        let bytes = prepared.encode();
        let decoded = crate::PreparedPackage::decode_expected(&bytes, &prepared.key).unwrap();
        let mut source = BundleSource::new();
        decoded.mount_into(&mut source);
        let layer = &source.layers["p#1"];
        match &layer.files {
            LayerFiles::Compressed { names, .. } => assert_eq!(names.len(), 4),
            LayerFiles::Owned(_) => panic!("prepared mount unexpectedly materialized files"),
        }
        let second = source.cache_root().join("p#1/package/second.txt");
        assert_eq!(source.read(&second).unwrap(), b"second");
    }

    #[test]
    fn read_cache_fork_shares_carrier_and_indexes_but_isolates_observation_state() {
        let prepared = crate::PreparedPackage::prepare(
            "p#1",
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"p","version":"1"}"#.to_vec(),
                ),
                ("first.txt".into(), b"first".to_vec()),
                ("second.txt".into(), b"second".to_vec()),
            ]),
        )
        .unwrap();
        let encoded = prepared.encode();
        let decoded = crate::PreparedPackage::decode_expected(&encoded, &prepared.key).unwrap();
        let mut retained = BundleSource::new();
        decoded.mount_into(&mut retained);
        retained
            .read(&retained.cache_root().join("p#1/package/first.txt"))
            .unwrap();
        let retained_before = retained.compression_metrics();
        assert!(retained_before.cached_raw_bytes > 0);

        let working = retained.fork_read_cache();
        let working_before = working.compression_metrics();
        assert_eq!(
            working_before.compressed_retained_bytes,
            retained_before.compressed_retained_bytes
        );
        assert_eq!(working_before.cached_raw_bytes, 0);
        let (
            LayerFiles::Compressed {
                files: retained_files,
                names: retained_names,
            },
            LayerFiles::Compressed {
                files: working_files,
                names: working_names,
            },
        ) = (&retained.layers["p#1"].files, &working.layers["p#1"].files)
        else {
            panic!("prepared cache fork unexpectedly materialized files")
        };
        assert!(Rc::ptr_eq(retained_names, working_names));
        assert!(retained_files.shares_member_index_with(working_files));
        assert!(Rc::ptr_eq(
            &retained.layers["p#1"].dirs,
            &working.layers["p#1"].dirs
        ));
        assert!(retained_files.shares_backing_bytes_with(working_files));
        assert!(!retained_files.shares_read_cache_with(working_files));

        working
            .read(&working.cache_root().join("p#1/package/second.txt"))
            .unwrap();
        assert!(working.compression_metrics().cached_raw_bytes > 0);
        assert_eq!(retained.compression_metrics(), retained_before);
    }
}
