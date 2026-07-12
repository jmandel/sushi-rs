//! `wasm_api` — the wasm-bindgen JS surface for the FSH editor. It keeps
//! `wasm-bindgen` OUT of the core crates: the compiler and the
//! snapshot walk engine stay bindgen-free and native-tested; this crate is the
//! only place JS types are marshalled.
//!
//! # The session surface (preferred)
//!
//! One handle — [`Session`] — with grouped methods, ONE error envelope and ONE
//! JSON result envelope (both `apiVersion`-stamped). Construct it once in the
//! Worker and call methods on it:
//!
//! ```js
//! const s = new Session();
//! s.mount(bundlesJson);                  // -> { ok, apiVersion, result: { mounted } }
//! s.prepareAndMount(bundlesJson);        // cold normalize/mount + artifact metadata
//! const artifact = s.takePrepared(label); // direct Uint8Array, intentionally not JSON
//! s.beginPreparedMount(count);           // warm all-or-nothing compact transaction
//! s.stagePreparedMount(bytes, key);      // one checked artifact at a time
//! s.commitPreparedMount();               // publish only after all stages validate
//! s.snapshot(urlOrInlineSd);
//! s.prepare(generatorSpecJson);
//! s.expandValueSet(vsJson, resourcesJson);
//! s.resolveProject(config, versionIndexJson);
//! Session.version();                     // static
//! ```
//!
//! Every metadata/content method returns a JSON string the Worker `JSON.parse`s.
//! The envelope is uniform:
//!   - success: `{ "apiVersion": 1, "ok": true,  "op": "<name>", "result": <payload> }`
//!   - failure: `{ "apiVersion": 1, "ok": false, "op": "<name>", "error": { "message": "…" } }`
//! Methods never throw for domain errors — they return `ok:false`. The one
//! deliberate exception is `takePrepared`: it moves a pending binary artifact
//! directly into a `Uint8Array`, and throws if the label is absent/already taken.
//!
//! The original flat exports and process-global engine are gone. Each Session
//! owns one compiler/package coordinator; prepared site runtimes are addressed
//! only by immutable SiteBuild handles.
//!
//! Everything runs synchronously in the Worker; the walk engine is the same code
//! the native gates exercise (proven byte-identical by `scripts/wasm-parity.sh`).
//!
//! ## Native build
//! The crate also builds on native targets (JS glue is inert there) so
//! `cargo test --workspace` links it. The real entry points are only meaningful
//! under `wasm32-unknown-unknown` + wasm-bindgen.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use package_store::{BundleSource, PackageSource};
use render_page::ArtifactObservation;

mod render_surface;
#[cfg(test)]
use render_surface::build_render_state;
use render_surface::{
    build_render_semantics, build_render_state_from_semantics, RenderState, SiteOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::prelude::*;

/// The result/error envelope + apiVersion are the SHARED implementation
/// (`api_envelope`) — one schema for the Session and the `fig` CLI's `--json`.
use api_envelope::{envelope, envelope_ser, API_VERSION};

/// Keep enough immutable generations for hot-reload comparison without
/// retaining an editing session's complete history of RenderStates and CAS
/// objects. This is preparation recency, not read recency: querying an old
/// handle never extends its lifetime.
const RETAINED_SITE_BUILD_LIMIT: usize = 2;

// ---------------------------------------------------------------------------
// A shareable package source. Store/context take `impl PackageSource + 'static`
// by value; we hand them a cheap `Rc<BundleSource>` clone so the (large) mounted
// package bytes are shared, not re-copied per call.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SharedBundle {
    source: Rc<BundleSource>,
    root: PathBuf,
    /// When present, hide every package directory outside this exact resolver
    /// fixpoint. This lets a compile use immutable selected coordinates even if
    /// additional versions are mounted in the session cache.
    allowed_labels: Option<BTreeSet<String>>,
}

impl SharedBundle {
    fn permits(&self, path: &std::path::Path) -> bool {
        let Some(allowed) = &self.allowed_labels else {
            return true;
        };
        if path == self.root {
            return true;
        }
        let Ok(relative) = path.strip_prefix(&self.root) else {
            return false;
        };
        let Some(label) = relative
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
        else {
            return false;
        };
        allowed.contains(label)
    }
}

impl PackageSource for SharedBundle {
    fn read(&self, path: &std::path::Path) -> std::io::Result<Vec<u8>> {
        if !self.permits(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "package is outside the compile resolver fixpoint",
            ));
        }
        self.source.read(path)
    }
    fn read_dir(&self, path: &std::path::Path) -> std::io::Result<Vec<package_store::DirEntry>> {
        if !self.permits(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "package is outside the compile resolver fixpoint",
            ));
        }
        let mut entries = self.source.read_dir(path)?;
        if path == self.root {
            if let Some(allowed) = &self.allowed_labels {
                entries.retain(|entry| allowed.contains(&entry.file_name));
            }
        }
        Ok(entries)
    }
    fn exists(&self, path: &std::path::Path) -> bool {
        self.permits(path) && self.source.exists(path)
    }
    fn is_dir(&self, path: &std::path::Path) -> bool {
        self.permits(path) && self.source.is_dir(path)
    }
    // write_new: default (read-only) — bundles ship the `.derived-index.json`.
}

// ---------------------------------------------------------------------------
// Engine — the mounted package source + last-compile locals. Each constructed
// `Session` owns exactly one Engine.
//
// Every operation is an inherent method returning `Result<_, String>` — plain
// Rust errors, no `JsError` (which panics off-wasm). Session does the JS
// marshalling.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Engine {
    /// The bundle source packages are mounted into, wrapped in an `Rc` so each
    /// `compile`/`snapshot` call shares the (large) mounted bytes with a cheap
    /// clone. `mount` appends lazily-fetched packages by rebuilding a clone and
    /// committing on success — so per-keystroke compiles never copy bundle bytes.
    bundle: Option<Rc<BundleSource>>,
    cache_root: PathBuf,
    /// The `<id>#<ver>` labels of the packages mounted, in mount order — the
    /// package list a `PackageContext` loads.
    packages: Vec<String>,
    /// Content-addressed metadata for the exact normalized bytes mounted under
    /// each package label. The mutable BundleSource is an execution cache; this
    /// map is the immutable package material used to construct a SiteBuild lock.
    package_materials: BTreeMap<String, MountedPackage>,
    /// Short-lived direct-binary exports produced by `prepareAndMount`. The JS
    /// host removes each with `takePrepared` immediately after persisting it.
    prepared_exports: BTreeMap<String, Vec<u8>>,
    /// A multi-call warm mount validates compact artifacts one at a time and
    /// commits them together. This avoids constructing a second whole-closure
    /// JavaScript batch while retaining the existing all-or-nothing law.
    prepared_mount: Option<PreparedMountTransaction>,
    /// Invalidates an in-flight staged transaction if another package mutation
    /// somehow interleaves through a non-browser host.
    package_generation: u64,
    /// The last package resolver fixpoint, bound to the config bytes it resolved.
    /// A SiteBuild may only claim this closure when its compile used those same
    /// config bytes.
    resolved_packages: Option<ResolvedPackages>,
    /// Current `compileProject()` outputs `(synthetic path, body)`, indexed as
    /// local resources for snapshot base resolution.
    last_compiled: Vec<(PathBuf, Value)>,
    /// Exact compiler identity installed in `last_compiled`. Site-file CONTENT
    /// is deliberately absent: only its page listing reaches IG export. The
    /// resolved package fixpoint is part of this key, so a semantically equal
    /// authored project under a different exact closure cannot reuse compiler
    /// state or retain downstream preparation caches.
    last_compile_key: Option<SemanticCompilationKey>,
    /// Exact authored inputs for the last successful `compileProject` call.
    last_project: Option<CompiledProjectRevision>,
    /// Serialized public result of the last successful semantic compile. This
    /// is retained beside `last_project` only so an exactly compiler-equivalent
    /// `compileProject` revision can return the same resources/diagnostics while
    /// replacing its authored site bytes. It is not a second build cache: the
    /// existing semantic/project identities are the sole reuse authority.
    last_compile_result: Option<CompileResult>,
    last_compile_diagnostics: Vec<DiagnosticJs>,
    /// The one exact successful semantic compilation immediately preceding the
    /// active fields above. A previous hit swaps the two payloads; a third
    /// distinct success replaces this value. Failed compiles do not touch it.
    previous_compilation: Option<SemanticCompilation>,
    /// Input page-folder listing (`input/{pagecontent,pages,resource-docs}` ->
    /// filenames) threaded into IG export so the generated IG's `definition.page`
    /// narrative tree is complete (narrative titles + the artifacts-section number).
    /// Set via `set_page_listing`; empty by default (artifact layer needs no pages).
    page_listing: std::collections::HashMap<String, Vec<String>>,
    /// Immutable target-specific site runtimes. A handle is exactly the closed
    /// SiteBuild id; mutable work below it is only path-local memoization and
    /// can never change the build that another handle names. Only the current
    /// and immediately previous successful preparations are retained.
    site_builds: BTreeMap<String, SiteBuildRuntime>,
    /// Oldest-to-newest successful preparation order for `site_builds`.
    site_build_generations: VecDeque<String>,
    /// Renderer-neutral semantics derived from the current (or an earlier)
    /// exact compile revision. The entry is reusable only when its canonical
    /// key matches every authored byte, locked package, compiled value,
    /// preparation option, and preparation recipe below.
    prepared_guide_cache: Option<PreparedGuideCacheEntry>,
    /// Snapshot-completed local resources are independent of authored prose,
    /// images, data, and includes. Retain exactly one set under a key binding
    /// the compiled resource values, exact package material/edges, resolver
    /// order, and snapshot recipe. A hit still re-runs authored augmentation
    /// and constructs a new complete PreparedGuide for the current revision.
    snapshot_completed_local_cache: Option<SnapshotCompletedLocalCacheEntry>,
    /// Complete external-builder handoff derived from one exact prepared-guide
    /// key plus target/diagnostics/projection recipe. Keeping the result (CAS
    /// objects included) avoids both semantic preparation and reprojection on a
    /// repeated request while never relying on a mutable "latest build" alias.
    closed_site_build_cache: Option<ClosedSiteBuildCacheEntry>,
    #[cfg(test)]
    derived_cache_hits: DerivedCacheHits,
}

struct PreparedMountTransaction {
    expected_packages: u32,
    base_generation: u64,
    packages: Vec<package_store::PreparedPackage>,
    artifact_bytes: u64,
    indexed_members: u64,
    decode_validate_ms: f64,
    base_compression: package_store::BundleCompressionMetrics,
}

#[derive(Clone)]
struct PreparedGuideCacheEntry {
    key: site_build::Sha256Digest,
    guide: site_build::PreparedGuide,
}

struct SnapshotCompletedLocalCacheEntry {
    key: site_build::Sha256Digest,
    generated: Vec<Value>,
    primary_implementation_guide: Value,
}

struct PreparationCacheKeys {
    prepared_guide: site_build::Sha256Digest,
    snapshot_completed_local: site_build::Sha256Digest,
}

#[derive(Clone, Debug, PartialEq)]
struct SemanticCompilationKey {
    semantic_inputs_sha256: site_build::Sha256Digest,
    resolved_packages: ResolvedPackages,
}

struct SemanticCompilation {
    key: SemanticCompilationKey,
    compiled: Vec<(PathBuf, Value)>,
    result: CompileResult,
    diagnostics: Vec<DiagnosticJs>,
}

#[derive(Clone)]
struct ClosedSiteBuildCacheEntry {
    key: site_build::Sha256Digest,
    projection: site_build::cycle_semantic::ClosedCycleProjection,
}

#[cfg(test)]
#[derive(Default)]
struct DerivedCacheHits {
    compile_project: u64,
    snapshot_completed_local: u64,
    prepared_guide: u64,
    closed_site_build: u64,
    retained_publisher_runtime: u64,
}

#[derive(Clone)]
struct PublisherBuildRuntime {
    /// Private identity of every input that can affect Publisher runtime,
    /// model, render-state, catalog, or SiteBuild assembly. This is only an
    /// index over the already-bounded retained runtimes, never a build value.
    preparation_key: site_build::Sha256Digest,
    state: Rc<RenderState>,
    runtime: Option<site_producer::publisher_runtime::PublisherRuntime>,
    build: site_build::ClosedSiteBuild,
    catalog: Vec<OutputDescriptor>,
    ready: BTreeMap<site_build::OutputPath, PreparedOutput>,
    objects: BTreeMap<site_build::Sha256Digest, Vec<u8>>,
    renderer: site_build::RendererImplementation,
    output_options: BTreeMap<String, String>,
}

#[derive(Clone)]
struct CycleBuildRuntime {
    build: site_build::ClosedSiteBuild,
    objects: BTreeMap<site_build::Sha256Digest, Vec<u8>>,
}

#[derive(Clone)]
enum SiteBuildRuntime {
    Publisher(PublisherBuildRuntime),
    Cycle(CycleBuildRuntime),
}

#[derive(Clone)]
struct PreparedOutput {
    content: site_build::ContentRef,
    producer: site_build::OutputProducer,
    source: Option<String>,
    owner: Option<site_build::OutputPath>,
}

#[derive(Clone, Debug)]
struct MountedPackage {
    content: site_build::ContentRef,
    declared_dependencies: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResolvedPackages {
    config_sha256: site_build::Sha256Digest,
    /// Exact package manifests read while proving the context closure. This
    /// includes R4-incompatible dependencies which the resolver deliberately
    /// inspected and excluded from the executable closure. Retaining these
    /// coordinates lets PackageLock distinguish an excluded declared edge from
    /// an unresolved/dangling one without adding the excluded package itself.
    resolution_support: BTreeSet<String>,
    labels: Vec<String>,
}

#[derive(Clone, Debug)]
struct CompiledProjectRevision {
    config: String,
    fsh: BTreeMap<String, String>,
    predefined: BTreeMap<String, Value>,
    site_files: BTreeMap<String, String>,
    /// Exact resolver fixpoint installed when this compile ran. A later
    /// resolveProject call, even for the same config/range, cannot silently
    /// change the package lock or snapshot context of this revision.
    resolved_packages: Option<ResolvedPackages>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct RenderSemanticInputs {
    config: String,
    fsh: BTreeMap<String, String>,
    predefined: BTreeMap<String, Value>,
    page_listing: BTreeMap<String, Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SemanticCompilationKeyPayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    engine_api: u32,
    inputs: &'a RenderSemanticInputs,
}

const SEMANTIC_COMPILATION_KEY_SCHEMA: &str = "semantic-compilation-key/v1";
const SEMANTIC_COMPILATION_RECIPE: &str = "sushi.compile-project/v1";

fn semantic_compilation_key(
    inputs: &RenderSemanticInputs,
    resolved_packages: &ResolvedPackages,
    operation: &str,
) -> Result<SemanticCompilationKey, String> {
    let semantic_inputs_sha256 = site_build::sha256_canonical(&SemanticCompilationKeyPayload {
        schema: SEMANTIC_COMPILATION_KEY_SCHEMA,
        recipe: SEMANTIC_COMPILATION_RECIPE,
        engine_api: API_VERSION,
        inputs,
    })
    .map_err(|error| format!("{operation}: hash semantic compilation key: {error}"))?;
    Ok(SemanticCompilationKey {
        semantic_inputs_sha256,
        resolved_packages: resolved_packages.clone(),
    })
}

fn set_panic_hook() {
    #[cfg(target_family = "wasm")]
    console_error_panic_hook::set_once();
}

impl Engine {
    /// Commit a successful compile generation. Exact render-set equality alone
    /// is insufficient: raw config/FSH/predefined/page-listing inputs are also
    /// part of the key so no relevant compiler context is hidden. Packages,
    /// txcache, active-tables, and run UUID are invalidated at their own mutation
    /// boundaries. Ordinary site bytes and template chrome are intentionally not
    /// FragmentEngine inputs.
    fn take_active_compilation(&mut self) -> Option<SemanticCompilation> {
        let key = self.last_compile_key.take()?;
        Some(SemanticCompilation {
            key,
            compiled: std::mem::take(&mut self.last_compiled),
            result: self
                .last_compile_result
                .take()
                .expect("an active compilation has a public result"),
            diagnostics: std::mem::take(&mut self.last_compile_diagnostics),
        })
    }

    /// Atomically make one already-successful semantic compilation current and
    /// retain exactly the displaced current payload. The caller constructs a
    /// fresh payload completely before this commit, so compiler failures cannot
    /// disturb the active or previous generation.
    fn replace_active_compilation(&mut self, next: SemanticCompilation) {
        let same_semantics = self.last_compile_key.as_ref() == Some(&next.key)
            && self.last_compiled == next.compiled;
        let previous = self.take_active_compilation();
        self.last_compile_key = Some(next.key);
        self.last_compiled = next.compiled;
        self.last_compile_result = Some(next.result);
        self.last_compile_diagnostics = next.diagnostics;
        self.previous_compilation = previous;
        if !same_semantics {
            self.snapshot_completed_local_cache = None;
            self.prepared_guide_cache = None;
            self.closed_site_build_cache = None;
        }
    }

    fn restore_cached_compilation(
        &mut self,
        key: &SemanticCompilationKey,
    ) -> Option<CompileResult> {
        if self.last_compile_key.as_ref() == Some(key) {
            let result = self.last_compile_result.clone();
            #[cfg(test)]
            {
                self.derived_cache_hits.compile_project += 1;
            }
            return result;
        }
        if self.previous_compilation.as_ref().map(|entry| &entry.key) != Some(key) {
            return None;
        }
        let previous = self
            .previous_compilation
            .take()
            .expect("previous compilation key matched");
        let result = previous.result.clone();
        self.replace_active_compilation(previous);
        #[cfg(test)]
        {
            self.derived_cache_hits.compile_project += 1;
        }
        Some(result)
    }

    /// Mount a set of bundles as the package cache, REPLACING any prior mount.
    /// Returns the number of packages mounted.
    fn init(&mut self, bundles_json: &str) -> Result<u32, String> {
        let parsed: Vec<BundleInput> = serde_json::from_str(bundles_json)
            .map_err(|e| format!("init: bad bundles JSON: {e}"))?;
        let mut src = BundleSource::new();
        let mut labels = Vec::new();
        let mut package_materials = BTreeMap::new();
        mount_into(
            &mut src,
            &parsed,
            &mut labels,
            &mut package_materials,
            "init",
        )?;
        self.cache_root = src.cache_root().to_path_buf();
        self.bundle = Some(Rc::new(src));
        self.packages = labels;
        self.package_materials = package_materials;
        self.prepared_exports.clear();
        self.prepared_mount = None;
        self.package_generation = self.package_generation.wrapping_add(1);
        self.resolved_packages = None;
        self.last_compiled.clear();
        self.last_compile_key = None;
        self.last_project = None;
        self.last_compile_result = None;
        self.last_compile_diagnostics.clear();
        self.previous_compilation = None;
        self.snapshot_completed_local_cache = None;
        self.prepared_guide_cache = None;
        self.closed_site_build_cache = None;
        Ok(parsed.len() as u32)
    }

    /// Mount ADDITIONAL bundles (lazy per-bundle loading, editor spec §1).
    /// Already-mounted labels are skipped (idempotent). Returns the total package
    /// count after mounting.
    ///
    /// Builds on a CLONE of the mounted state and only commits it AFTER a
    /// successful mount — so a mid-mount error (e.g. bad base64 in a lazily
    /// fetched bundle) leaves the existing state intact rather than uninitialized.
    fn mount(&mut self, bundles_json: &str) -> Result<u32, String> {
        let parsed: Vec<BundleInput> = serde_json::from_str(bundles_json)
            .map_err(|e| format!("mount_bundles: bad bundles JSON: {e}"))?;
        let mut src = (**self
            .bundle
            .as_ref()
            .ok_or("mount_bundles: engine not initialized; call init() first")?)
        .clone();
        let mut labels = self.packages.clone();
        let mut package_materials = self.package_materials.clone();
        let existing: BTreeSet<String> = labels.iter().cloned().collect();
        let mut transaction = BTreeSet::new();
        let mut fresh = Vec::new();
        for package in parsed {
            if existing.contains(&package.label) {
                continue;
            }
            if !transaction.insert(package.label.clone()) {
                return Err(format!(
                    "mount_bundles: duplicate new package label in one transaction: {}",
                    package.label
                ));
            }
            fresh.push(package);
        }
        // Fallible: on Err we return WITHOUT having touched our bundle/packages.
        let package_set_changed = !fresh.is_empty();
        mount_into(
            &mut src,
            &fresh,
            &mut labels,
            &mut package_materials,
            "mount_bundles",
        )?;
        // Commit only after success.
        self.cache_root = src.cache_root().to_path_buf();
        self.bundle = Some(Rc::new(src));
        let total = labels.len() as u32;
        self.packages = labels;
        self.package_materials = package_materials;
        if package_set_changed {
            self.package_generation = self.package_generation.wrapping_add(1);
            // A resolver fixpoint is a statement about the mounted candidate
            // set. Even if a new package looks unrelated, mutable/range
            // requests must be resolved again before another compileProject.
            self.resolved_packages = None;
        }
        Ok(total)
    }

    /// Mount one versioned binary PreparedPackage. Validation happens before
    /// any engine mutation; the artifact's current derived-index sidecar is
    /// mounted directly, so this path performs no resource-index rebuild.
    fn mount_prepared(&mut self, bytes: Vec<u8>, expected_key: &str) -> Result<u32, String> {
        let expected: package_store::PreparedPackageKey = expected_key
            .parse()
            .map_err(|error| format!("mountPrepared: invalid expected key: {error:#}"))?;
        let prepared = package_store::PreparedPackage::decode_owned(bytes, &expected)
            .map_err(|error| format!("mountPrepared: invalid artifact: {error:#}"))?;
        self.commit_prepared(vec![prepared], "mountPrepared")?;
        Ok(self.packages.len() as u32)
    }

    fn begin_prepared_mount(&mut self, expected_packages: u32) -> Result<(), String> {
        if self.bundle.is_none() {
            return Err("beginPreparedMount: engine not initialized; call init() first".into());
        }
        if expected_packages == 0 {
            return Err("beginPreparedMount: expected package count must be positive".into());
        }
        if self.prepared_mount.is_some() {
            return Err("beginPreparedMount: another prepared mount is already active".into());
        }
        self.prepared_mount = Some(PreparedMountTransaction {
            expected_packages,
            base_generation: self.package_generation,
            // `expected_packages` crosses the public WASM boundary. Grow only
            // as validated artifacts arrive instead of trusting it as an eager
            // allocation size (a forged u32::MAX must not trap the worker).
            packages: Vec::new(),
            artifact_bytes: 0,
            indexed_members: 0,
            decode_validate_ms: 0.0,
            base_compression: self
                .bundle
                .as_ref()
                .map(|source| source.compression_metrics())
                .unwrap_or_default(),
        });
        Ok(())
    }

    fn stage_prepared_mount(
        &mut self,
        bytes: Vec<u8>,
        expected_key: &str,
    ) -> Result<PreparedStageResult, String> {
        let transaction = self
            .prepared_mount
            .as_mut()
            .ok_or("stagePreparedMount: no prepared mount is active")?;
        if transaction.packages.len() >= transaction.expected_packages as usize {
            return Err("stagePreparedMount: received more packages than declared".into());
        }
        let expected: package_store::PreparedPackageKey = expected_key
            .parse()
            .map_err(|error| format!("stagePreparedMount: invalid expected key: {error:#}"))?;
        let artifact_bytes = bytes.len() as u64;
        let started = clock_ms();
        let package = package_store::PreparedPackage::decode_owned(bytes, &expected)
            .map_err(|error| format!("stagePreparedMount: invalid artifact: {error:#}"))?;
        let decode_validate_ms = (clock_ms() - started).max(0.0);
        if transaction
            .packages
            .iter()
            .any(|prior| prior.label == package.label)
        {
            return Err(format!(
                "stagePreparedMount: duplicate package label {}",
                package.label
            ));
        }
        let indexed_members = package.files.len() as u64;
        let label = package.label.clone();
        transaction.artifact_bytes = transaction.artifact_bytes.saturating_add(artifact_bytes);
        transaction.indexed_members = transaction.indexed_members.saturating_add(indexed_members);
        transaction.decode_validate_ms += decode_validate_ms;
        transaction.packages.push(package);
        Ok(PreparedStageResult {
            label,
            staged: transaction.packages.len() as u32,
            artifact_bytes,
            indexed_members,
            decode_validate_ms,
        })
    }

    fn commit_prepared_mount(&mut self) -> Result<PackageMountResult, String> {
        let transaction = self
            .prepared_mount
            .take()
            .ok_or("commitPreparedMount: no prepared mount is active")?;
        if transaction.base_generation != self.package_generation {
            return Err(
                "commitPreparedMount: mounted package state changed during transaction".into(),
            );
        }
        if transaction.packages.len() != transaction.expected_packages as usize {
            return Err(format!(
                "commitPreparedMount: expected {} packages, staged {}",
                transaction.expected_packages,
                transaction.packages.len()
            ));
        }
        let started = clock_ms();
        let added = self.commit_prepared(transaction.packages, "commitPreparedMount")?;
        let compression = compression_delta(
            transaction.base_compression,
            self.bundle
                .as_ref()
                .map(|source| source.compression_metrics())
                .unwrap_or_default(),
        );
        let mount_ms = (clock_ms() - started).max(0.0);
        Ok(PackageMountResult {
            mounted: self.packages.len() as u32,
            added,
            packages: transaction.expected_packages,
            manifest_json_bytes: 0,
            artifact_bytes: transaction.artifact_bytes,
            retained_blob_bytes: transaction.artifact_bytes,
            indexed_members: transaction.indexed_members,
            member_body_copies: 0,
            manifest_parse_ms: 0.0,
            decode_validate_ms: transaction.decode_validate_ms,
            mount_ms,
            compression,
        })
    }

    fn abort_prepared_mount(&mut self) -> bool {
        self.prepared_mount.take().is_some()
    }

    fn package_storage_metrics(&self) -> package_store::BundleCompressionMetrics {
        self.bundle
            .as_ref()
            .map(|source| source.compression_metrics())
            .unwrap_or_default()
    }

    /// Cold path: turn the existing inflated JSON/base64 input into the exact
    /// binary cache artifact while mounting the same prepared material once.
    /// `takePrepared(label)` transfers each artifact to JS without base64.
    fn prepare_and_mount(&mut self, bundles_json: &str) -> Result<PrepareMountResult, String> {
        if self.bundle.is_none() {
            return Err("prepareAndMount: engine not initialized; call init() first".into());
        }
        let started = clock_ms();
        let parsed: Vec<BundleInput> = serde_json::from_str(bundles_json)
            .map_err(|error| format!("prepareAndMount: bad bundles JSON: {error}"))?;
        let parsed_at = clock_ms();
        if parsed.is_empty() {
            return Err("prepareAndMount: no packages supplied".into());
        }
        let mut transaction = BTreeSet::new();
        let mut artifacts = Vec::with_capacity(parsed.len());
        let mut prepared = Vec::with_capacity(parsed.len());
        let mut pending = BTreeMap::new();
        let mut prepared_members = 0u64;
        let mut base64_bytes = 0u64;
        let mut decoded_source_bytes = 0u64;
        let mut normalized_bytes = 0u64;
        let mut artifact_bytes = 0u64;
        let mut base64_decode_ms = 0.0f64;
        let mut normalization_ms = 0.0f64;
        let mut indexing_ms = 0.0f64;
        let mut artifact_encode_ms = 0.0f64;
        for package in parsed {
            if !transaction.insert(package.label.clone()) {
                return Err(format!(
                    "prepareAndMount: duplicate package label in one transaction: {}",
                    package.label
                ));
            }
            let mut entries = BTreeMap::new();
            for (name, b64) in package.files {
                base64_bytes = base64_bytes.saturating_add(b64.len() as u64);
                let decode_started = clock_ms();
                let body = base64_decode(&b64)
                    .map_err(|error| format!("prepareAndMount: bad base64 for {name}: {error}"))?;
                base64_decode_ms += (clock_ms() - decode_started).max(0.0);
                decoded_source_bytes = decoded_source_bytes.saturating_add(body.len() as u64);
                entries.insert(name, body);
            }
            let normalize_started = clock_ms();
            let builder = package_store::PreparedPackage::normalize(&package.label, entries)
                .map_err(|error| format!("prepareAndMount: invalid package: {error:#}"))?;
            normalization_ms += (clock_ms() - normalize_started).max(0.0);
            let index_started = clock_ms();
            let package = builder
                .build()
                .map_err(|error| format!("prepareAndMount: invalid package: {error:#}"))?;
            indexing_ms += (clock_ms() - index_started).max(0.0);
            normalized_bytes = normalized_bytes.saturating_add(package.files.raw_bytes());
            let encode_started = clock_ms();
            let bytes = package.encode();
            artifact_encode_ms += (clock_ms() - encode_started).max(0.0);
            prepared_members += package.files.len() as u64;
            artifact_bytes += bytes.len() as u64;
            artifacts.push(PreparedExport {
                label: package.label.clone(),
                cache_key: package.key.cache_key(),
                artifact_sha256: site_build::Sha256Digest::of_bytes(&bytes).to_string(),
                bytes: bytes.len() as u64,
            });
            prepared.push(package);
            pending.insert(artifacts.last().unwrap().label.clone(), bytes);
        }
        let decoded = clock_ms();
        // Do not expose artifacts from a transaction whose mount fails.
        let added = self.commit_prepared(prepared, "prepareAndMount")?;
        self.prepared_exports.extend(pending);
        let finished = clock_ms();
        Ok(PrepareMountResult {
            mounted: self.packages.len() as u32,
            added,
            artifacts,
            artifact_bytes,
            prepared_members,
            input_json_bytes: bundles_json.len() as u64,
            base64_bytes,
            decoded_source_bytes,
            normalized_bytes,
            mount_member_body_copies: 0,
            json_parse_ms: (parsed_at - started).max(0.0),
            base64_decode_ms,
            normalization_ms,
            indexing_ms,
            artifact_encode_ms,
            decode_validate_prepare_ms: (decoded - started).max(0.0),
            mount_ms: (finished - decoded).max(0.0),
        })
    }

    fn take_prepared(&mut self, label: &str) -> Result<Vec<u8>, String> {
        self.prepared_exports
            .remove(label)
            .ok_or_else(|| format!("takePrepared: no pending artifact for {label}"))
    }

    /// Append decoded packages as immutable layers. All fallible validation and
    /// conflict checks precede the shallow BundleSource clone and infallible
    /// layer appends, preserving all-or-nothing mount semantics.
    fn commit_prepared(
        &mut self,
        prepared: Vec<package_store::PreparedPackage>,
        operation: &str,
    ) -> Result<u32, String> {
        let base = self
            .bundle
            .as_ref()
            .ok_or_else(|| format!("{operation}: engine not initialized; call init() first"))?;
        let mut transaction = BTreeSet::new();
        let mut contents = Vec::with_capacity(prepared.len());
        for package in &prepared {
            if !transaction.insert(package.label.clone()) {
                return Err(format!(
                    "{operation}: duplicate package label in one transaction: {}",
                    package.label
                ));
            }
            let content = prepared_content(package, operation)?;
            if let Some(existing) = self.package_materials.get(&package.label) {
                if existing.content != content
                    || existing.declared_dependencies != package.declared_dependencies
                {
                    return Err(format!(
                        "{operation}: package label {} is already mounted with different content",
                        package.label
                    ));
                }
            }
            contents.push(content);
        }

        let mut source = (**base).clone(); // shallow: immutable layer Rc clones only
        let mut added = 0u32;
        for (package, content) in prepared.into_iter().zip(contents) {
            if self.package_materials.contains_key(&package.label) {
                continue;
            }
            let mounted = package.mount_into(&mut source);
            self.packages.push(mounted.label.clone());
            self.package_materials.insert(
                mounted.label,
                MountedPackage {
                    content,
                    declared_dependencies: mounted.declared_dependencies,
                },
            );
            added += 1;
        }
        if added > 0 {
            self.package_generation = self.package_generation.wrapping_add(1);
            self.cache_root = source.cache_root().to_path_buf();
            self.bundle = Some(Rc::new(source));
            self.resolved_packages = None;
        }
        Ok(added)
    }

    /// The shared package source + cache root + package labels for a call. Cheap:
    /// an `Rc` refcount bump, so the mounted bytes are shared, never copied.
    fn source(&self) -> Result<(SharedBundle, PathBuf, Vec<String>), String> {
        let bundle = self
            .bundle
            .clone()
            .ok_or("engine not initialized: call init(bundles) first")?;
        Ok((
            SharedBundle {
                source: bundle,
                root: self.cache_root.clone(),
                allowed_labels: None,
            },
            self.cache_root.clone(),
            self.packages.clone(),
        ))
    }

    /// A read-only package view containing exactly one previously resolved
    /// fixpoint. The bytes remain shared; only directory visibility is scoped.
    fn source_for_resolved(
        &self,
        resolved: &ResolvedPackages,
    ) -> Result<(SharedBundle, PathBuf, Vec<String>), String> {
        let bundle = self
            .bundle
            .clone()
            .ok_or("engine not initialized: call init(bundles) first")?;
        let mut allowed_labels = BTreeSet::new();
        for label in &resolved.labels {
            if !self.packages.contains(label) || !self.package_materials.contains_key(label) {
                return Err(format!(
                    "resolved package {label} is not present in the mounted package store"
                ));
            }
            if !allowed_labels.insert(label.clone()) {
                return Err(format!("resolved package closure repeats {label}"));
            }
        }
        Ok((
            SharedBundle {
                source: bundle,
                root: self.cache_root.clone(),
                allowed_labels: Some(allowed_labels),
            },
            self.cache_root.clone(),
            resolved.labels.clone(),
        ))
    }

    /// Package view for operations derived from the current compile. A complete
    /// `compileProject` revision remains bound to its captured resolver closure
    /// even if unrelated/template/other-version packages are mounted later.
    /// Internal non-project revisions have no such certificate and retain the
    /// historical all-mounted behavior for snapshot-only operations.
    fn source_for_current_revision(&self) -> Result<(SharedBundle, PathBuf, Vec<String>), String> {
        match self
            .last_project
            .as_ref()
            .and_then(|project| project.resolved_packages.as_ref())
        {
            Some(resolved) => self.source_for_resolved(resolved),
            None => self.source(),
        }
    }

    fn compile_with_resolved(
        &self,
        files_json: &str,
        config: &str,
        predefined_json: &str,
        resolved: &ResolvedPackages,
        key: SemanticCompilationKey,
    ) -> Result<SemanticCompilation, String> {
        let (source, cache_root, _packages) = self.source_for_resolved(resolved)?;

        // FSH files: object -> Vec sorted by path (matches the disk walk order).
        let files_map: std::collections::BTreeMap<String, String> =
            serde_json::from_str(files_json)
                .map_err(|e| format!("compile: bad files JSON: {e}"))?;
        let fsh_files: Vec<(String, String)> = files_map
            .iter()
            .map(|(path, source)| (path.clone(), source.clone()))
            .collect();

        // Authored local resources: object path -> body. Preserve stock SUSHI's
        // fixed input-directory order rather than lexical map order: examples
        // deliberately follow resources and therefore win later duplicate-
        // identity projection exactly as they do on disk.
        let predefined_map: BTreeMap<String, Value> = if predefined_json.trim().is_empty() {
            BTreeMap::new()
        } else {
            serde_json::from_str(predefined_json)
                .map_err(|e| format!("compile: bad predefined JSON: {e}"))?
        };
        let predefined = ordered_predefined_resources(&predefined_map);
        let cache = cache_root.to_string_lossy().into_owned();
        // Keep the predefined bodies: the compiler consumes them for resolution
        // but does NOT re-emit them as `compiled` output. A predefined-resource IG
        // (0 FSH; conformance lives under input/resources/**, e.g. US Core / IPS)
        // would otherwise leave the render set holding only the ImplementationGuide
        // — every profile fragment misses. They ARE part of the publisher `output/`
        // tree, so they belong in the render set (/own) too.
        let predefined_for_render = predefined.clone();
        // Generate the REAL ImplementationGuide (byte-identical to the disk build's
        // ImplementationGuide-<id>.json) alongside the conformance resources, so the
        // render set carries a faithful IG — correct artifact/example titles and
        // example markers — instead of the minimal prepare(publisher) synthesis. The
        // page-folder listing (for definition.page narrative pages) is threaded via
        // `self.page_listing`; empty is fine for the artifact layer.
        let (compiled, ig_resource, diagnostics) = compiler::build_project_in_memory_with_ig(
            config,
            &fsh_files,
            predefined,
            source,
            &cache,
            self.page_listing.clone(),
        )
        .map_err(|e| format!("compile failed: {e:#}"))?;

        // Stash the render set as snapshot-resolution locals: the FSH compile
        // output FIRST, then the predefined conformance/example resources keyed
        // by their publisher filename (`output/{Type}-{id}.json` parity). Both
        // land in the render surface's /own dir and act as base-resolution locals.
        let mut render_set: Vec<(PathBuf, Value)> = compiled
            .iter()
            .map(|r| {
                (
                    PathBuf::from(format!("/__compiled__/{}", r.filename)),
                    r.body.clone(),
                )
            })
            .collect();
        for (path, body) in &predefined_for_render {
            render_set.push((predefined_render_path(path)?, body.clone()));
        }
        // The generated ImplementationGuide joins the render set so prepare(publisher)
        // finds a faithful IG (correct titles/example markers/page tree) rather than
        // falling back to the minimal synthesis.
        if let Some(ig) = ig_resource {
            render_set.push((PathBuf::from(format!("/__ig__/{}", ig.filename)), ig.body));
        }
        let resources: Vec<CompiledResourceJs> =
            compiled.into_iter().map(CompiledResourceJs::from).collect();

        let diagnostics: Vec<DiagnosticJs> = diagnostics
            .into_iter()
            .map(|d| DiagnosticJs {
                severity: d.severity.to_string(),
                message: d.message,
                file: d.file,
                line: d.line,
            })
            .collect();

        let result = CompileResult {
            resources,
            diagnostics: diagnostics.clone(),
            timings: Timings::default(),
        };
        Ok(SemanticCompilation {
            key,
            compiled: render_set,
            result,
            diagnostics,
        })
    }

    /// Compile with the authored site-file manifest in scope. This is the normal
    /// editor build entry point: page-folder names must reach IG export during the
    /// ONE compile, so the later SiteBuild projection can consume `last_compiled`
    /// without recompiling merely to recover `definition.page`.
    fn compile_project(
        &mut self,
        files_json: &str,
        config: &str,
        predefined_json: &str,
        site_files_json: &str,
    ) -> Result<CompileResult, String> {
        let fsh: BTreeMap<String, String> = serde_json::from_str(files_json)
            .map_err(|e| format!("compileProject: bad FSH files JSON: {e}"))?;
        let predefined: BTreeMap<String, Value> = if predefined_json.trim().is_empty() {
            BTreeMap::new()
        } else {
            serde_json::from_str(predefined_json)
                .map_err(|e| format!("compileProject: bad predefined JSON: {e}"))?
        };
        let site_files: BTreeMap<String, String> = serde_json::from_str(site_files_json)
            .map_err(|e| format!("compileProject: bad site-files JSON: {e}"))?;
        validate_predefined_site_overlap(&predefined, &site_files, "compileProject")?;
        let config_digest = site_build::Sha256Digest::of_bytes(config.as_bytes());
        let resolved_packages = self
            .resolved_packages
            .as_ref()
            .filter(|resolved| resolved.config_sha256 == config_digest)
            .cloned()
            .ok_or_else(|| {
                "compileProject: no satisfied package resolver fixpoint for these config bytes; call resolveProject after the latest mount"
                    .to_string()
            })?;
        let page_listing = page_listing_from_site_files(&site_files);
        let semantic_inputs = RenderSemanticInputs {
            config: config.to_string(),
            fsh: fsh.clone(),
            predefined: predefined.clone(),
            page_listing: page_listing
                .iter()
                .map(|(directory, names)| (directory.clone(), names.clone()))
                .collect(),
        };
        let key = semantic_compilation_key(&semantic_inputs, &resolved_packages, "compileProject")?;
        let previous_page_listing = std::mem::replace(&mut self.page_listing, page_listing);
        if let Some(result) = self.restore_cached_compilation(&key) {
            self.last_project = Some(CompiledProjectRevision {
                config: config.to_string(),
                fsh,
                predefined,
                site_files,
                resolved_packages: Some(resolved_packages),
            });
            return Ok(result);
        }
        let compilation = self.compile_with_resolved(
            files_json,
            config,
            predefined_json,
            &resolved_packages,
            key,
        );
        if compilation.is_err() {
            self.page_listing = previous_page_listing;
        }
        let compilation = compilation?;
        let result = compilation.result.clone();
        self.replace_active_compilation(compilation);
        self.last_project = Some(CompiledProjectRevision {
            config: config.to_string(),
            fsh,
            predefined,
            site_files,
            resolved_packages: Some(resolved_packages),
        });
        Ok(result)
    }

    /// Build a fresh `PackageContext` over the last complete project's exact
    /// resolved closure plus its local resources. Explicit legacy compile/local
    /// modes retain the historical all-mounted context.
    fn build_context(&self) -> Result<snapshot_gen::PackageContext, String> {
        let (source, cache_root, packages) = self.source_for_current_revision()?;
        let mut ctx = snapshot_gen::PackageContext::new_with(source, &cache_root, &packages)
            .map_err(|e| format!("package context: {e:#}"))?;
        ctx.load_local_resources(self.last_compiled.clone());
        Ok(ctx)
    }

    /// Generate a snapshot for an inline SD JSON or a canonical URL/id/name.
    /// Returns the [`SnapshotResult`] payload (never a hard error for a missing
    /// profile — that lands in `messages`).
    fn snapshot(&self, input: &str) -> Result<SnapshotResult, String> {
        let ctx = self.build_context()?;

        // Inline SD if it parses as an object with resourceType StructureDefinition;
        // otherwise treat `input` as a URL/id/name and resolve it from local + pkgs.
        let derived: Value = match serde_json::from_str::<Value>(input.trim()) {
            Ok(v)
                if v.get("resourceType").and_then(|r| r.as_str())
                    == Some("StructureDefinition") =>
            {
                v
            }
            _ => {
                let query = input.trim();
                match ctx.fetch(query) {
                    Some(rc) => (*rc).clone(),
                    None => {
                        return Ok(SnapshotResult {
                            snapshot: None,
                            messages: vec![format!("no StructureDefinition found for '{query}'")],
                        });
                    }
                }
            }
        };

        Ok(
            match snapshot_gen::generate_snapshot(derived, &ctx, Default::default()) {
                Ok(v) => SnapshotResult {
                    snapshot: Some(v),
                    messages: Vec::new(),
                },
                Err(e) => SnapshotResult {
                    snapshot: None,
                    messages: vec![format!("{e:#}")],
                },
            },
        )
    }

    /// Tier-1 in-engine ValueSet expansion (spec §6). Returns the raw payload
    /// `Value` (either `{ ok:true, expansion, usedCodeSystems, copyright }` or
    /// `{ ok:false, notEnumerable }`).
    fn expand_valueset(&self, valueset_json: &str, resources_json: &str) -> Result<Value, String> {
        use compiler::terminology::{
            expand_enumerable as expand, MapResolver, NotEnumerable, RefusalKind,
        };

        let vs: Value = serde_json::from_str(valueset_json)
            .map_err(|e| format!("expand_enumerable: bad ValueSet JSON: {e}"))?;

        // `resources_json` may be an array (preferred) or an object map path->body
        // (accepted for convenience — the editor's predefined map shape).
        let mut resolver = MapResolver::new();
        let parsed: Value = if resources_json.trim().is_empty() {
            Value::Array(Vec::new())
        } else {
            serde_json::from_str(resources_json)
                .map_err(|e| format!("expand_enumerable: bad resources JSON: {e}"))?
        };
        match parsed {
            Value::Array(items) => {
                for r in items {
                    resolver.insert(r);
                }
            }
            Value::Object(map) => {
                for (_k, r) in map {
                    resolver.insert(r);
                }
            }
            _ => return Err("expand_enumerable: resources must be a JSON array or object".into()),
        }

        Ok(match expand(&vs, &resolver) {
            Ok(exp) => {
                let expansion = exp.to_expansion_json();
                // Lift used-codesystems out of expansion.parameter for the editor's
                // "code system versions" table (it also stays in parameter[]).
                let used: Vec<Value> = expansion
                    .get("parameter")
                    .and_then(Value::as_array)
                    .map(|params| {
                        params
                            .iter()
                            .filter(|p| {
                                p.get("name").and_then(Value::as_str) == Some("used-codesystem")
                            })
                            .filter_map(|p| p.get("valueUri").and_then(Value::as_str))
                            .map(|uri| match uri.split_once('|') {
                                Some((sys, ver)) => {
                                    serde_json::json!({ "system": sys, "version": ver })
                                }
                                None => serde_json::json!({ "system": uri }),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                serde_json::json!({
                    "ok": true,
                    "expansion": expansion,
                    "usedCodeSystems": used,
                    "copyright": exp.copyright(),
                })
            }
            Err(ne @ NotEnumerable { .. }) => {
                let kind = match ne.kind {
                    RefusalKind::ExternalSystemFilter => "ExternalSystemFilter",
                    RefusalKind::UnresolvableOrIncompleteSystem => "UnresolvableOrIncompleteSystem",
                    RefusalKind::UnresolvableValueSet => "UnresolvableValueSet",
                    RefusalKind::NestedNotEnumerable => "NestedNotEnumerable",
                    RefusalKind::UnsupportedLocalFilter => "UnsupportedLocalFilter",
                    RefusalKind::Malformed => "Malformed",
                    RefusalKind::CycleGuard => "CycleGuard",
                };
                serde_json::json!({
                    "ok": false,
                    "notEnumerable": {
                        "component": ne.component,
                        "index": ne.index,
                        "system": ne.system,
                        "kind": kind,
                        // The verbatim single-line refusal (Display = "component[i]: reason").
                        "reason": ne.reason,
                        "display": ne.to_string(),
                    }
                })
            }
        })
    }

    /// Build the callback-free Cycle v2 handoff from the exact preceding
    /// compile. This cannot invoke a second compile or a request-time fragment
    /// callback.
    fn prepare_cycle(
        &mut self,
        options: &PrepareGuideOptions,
    ) -> Result<
        (
            site_build::cycle_semantic::ClosedCycleProjection,
            PrepareMetrics,
        ),
        String,
    > {
        let total_started = clock_ms();
        let mut metrics = PrepareMetrics::default();
        let operation = "prepare(cycle)";
        let project = self.last_project.clone().ok_or_else(|| {
            format!("{operation}: compileProject has not established a complete source revision")
        })?;
        let (project_id, fhir_version) = compiled_ig_identity(&self.last_compiled)?;
        let started = clock_ms();
        let project_revision = self.site_build_project_revision(&project, &project_id)?;
        metrics.project_revision_ms = (clock_ms() - started).max(0.0);
        let started = clock_ms();
        let package_lock = self.site_build_package_lock(&project)?;
        metrics.package_lock_ms = (clock_ms() - started).max(0.0);
        let started = clock_ms();
        let resolved = project.resolved_packages.as_ref().ok_or_else(|| {
            format!("{operation}: compiled revision has no bound package resolver fixpoint")
        })?;
        let cache_keys = self.preparation_cache_keys(
            options,
            &project_revision,
            &package_lock,
            resolved,
            operation,
        )?;
        let prepared_key = cache_keys.prepared_guide;
        let snapshot_cache_key = cache_keys.snapshot_completed_local;
        metrics.snapshot_completed_local_cache_hit = self
            .snapshot_completed_local_cache
            .as_ref()
            .is_some_and(|entry| entry.key == snapshot_cache_key);
        metrics.prepared_guide_key_ms = (clock_ms() - started).max(0.0);

        let diagnostics = site_build_diagnostics(&self.last_compile_diagnostics);

        let mut parameters = BTreeMap::from([
            ("contract".into(), site_build::cycle_semantic::TARGET.into()),
            (
                "buildEpochSecs".into(),
                options.build_epoch_secs.to_string(),
            ),
            (
                "liquidAssetDirs".into(),
                if options.liquid_asset_dirs.is_empty() {
                    "input/includes".into()
                } else {
                    options.liquid_asset_dirs.join(",")
                },
            ),
        ]);
        if let Some(branch) = &options.branch {
            parameters.insert("branch".into(), branch.clone());
        }
        if let Some(revision) = &options.revision {
            parameters.insert("revision".into(), revision.clone());
        }

        let render_target = site_build::RenderTarget {
            renderer: site_build::ProducerRef::new("cycle-site", "2"),
            mode: site_build::RenderMode::ExternalBuilder,
            fhir_version,
            template: None,
            parameters,
        };

        let closed_key = closed_site_build_cache_key(&prepared_key, &render_target, &diagnostics)
            .map_err(|error| format!("{operation}: cache key: {error}"))?;
        if let Some(entry) = &self.closed_site_build_cache {
            if entry.key == closed_key {
                metrics.site_build_cache_hit = true;
                metrics.prepared_guide_cache_hit = self
                    .prepared_guide_cache
                    .as_ref()
                    .is_some_and(|prepared| prepared.key == prepared_key);
                metrics.total_ms = (clock_ms() - total_started).max(0.0);
                #[cfg(test)]
                {
                    self.derived_cache_hits.closed_site_build += 1;
                }
                return Ok((entry.projection.clone(), metrics));
            }
        }

        metrics.prepared_guide_cache_hit = self
            .prepared_guide_cache
            .as_ref()
            .is_some_and(|prepared| prepared.key == prepared_key);
        let started = clock_ms();
        let prepared_guide =
            self.site_model_from_compile(options, operation, prepared_key, snapshot_cache_key)?;
        metrics.prepared_guide_ms = (clock_ms() - started).max(0.0);
        let started = clock_ms();
        let input = site_build::cycle_semantic::CycleProjectionInput {
            project: project_revision,
            package_lock,
            render_target,
            diagnostics,
        };
        let projection = site_build::cycle_semantic::close_prepared(&prepared_guide, input)
            .map_err(|e| format!("{operation}: {e}"))?;
        metrics.catalog_ms = (clock_ms() - started).max(0.0);
        self.closed_site_build_cache = Some(ClosedSiteBuildCacheEntry {
            key: closed_key,
            projection: projection.clone(),
        });
        metrics.total_ms = (clock_ms() - total_started).max(0.0);
        Ok((projection, metrics))
    }

    /// Compute the whole-PreparedGuide and narrower snapshot-local identities
    /// from one canonical hash of the compiled revision. Both are private cache
    /// indexes; neither is a domain handoff or authority independent of the
    /// value it reconstructs.
    fn preparation_cache_keys(
        &self,
        input: &PrepareGuideOptions,
        project: &site_build::ProjectRevision,
        package_lock: &site_build::PackageLock,
        resolved: &ResolvedPackages,
        operation: &str,
    ) -> Result<PreparationCacheKeys, String> {
        let compiled_sha256 = self.compiled_revision_sha256(operation)?;
        let prepared_guide = site_build::sha256_canonical(&PreparedGuideCachePayload {
            schema: PREPARED_GUIDE_CACHE_SCHEMA,
            recipe: PREPARED_GUIDE_RECIPE,
            engine_api: API_VERSION,
            project,
            package_lock,
            compiled_sha256: &compiled_sha256,
            build_epoch_secs: input.build_epoch_secs,
            liquid_asset_dirs: &input.liquid_asset_dirs,
            branch: input.branch.as_deref(),
            revision: input.revision.as_deref(),
        })
        .map_err(|error| format!("{operation}: hash PreparedGuide cache key: {error}"))?;
        let snapshot_completed_local =
            site_build::sha256_canonical(&SnapshotCompletedLocalCachePayload {
                schema: SNAPSHOT_COMPLETED_LOCAL_CACHE_SCHEMA,
                recipe: SNAPSHOT_COMPLETED_LOCAL_RECIPE,
                engine_api: API_VERSION,
                compiled_sha256: &compiled_sha256,
                package_lock,
                resolved_config_sha256: &resolved.config_sha256,
                resolution_support: &resolved.resolution_support,
                resolved_labels: &resolved.labels,
            })
            .map_err(|error| {
                format!("{operation}: hash snapshot-completed local cache key: {error}")
            })?;
        Ok(PreparationCacheKeys {
            prepared_guide,
            snapshot_completed_local,
        })
    }

    fn compiled_revision_sha256(
        &self,
        operation: &str,
    ) -> Result<site_build::Sha256Digest, String> {
        let compiled: BTreeMap<String, &Value> = self
            .last_compiled
            .iter()
            .map(|(path, body)| {
                path.to_str()
                    .map(|path| (path.to_string(), body))
                    .ok_or_else(|| format!("{operation}: compiled path is not UTF-8: {path:?}"))
            })
            .collect::<Result<_, _>>()?;
        if compiled.len() != self.last_compiled.len() {
            return Err(format!(
                "{operation}: compiled revision contains duplicate paths"
            ));
        }
        site_build::sha256_canonical(&compiled)
            .map_err(|error| format!("{operation}: hash compiled revision: {error}"))
    }

    fn site_model_from_compile(
        &mut self,
        input: &PrepareGuideOptions,
        operation: &str,
        cache_key: site_build::Sha256Digest,
        snapshot_cache_key: site_build::Sha256Digest,
    ) -> Result<site_build::PreparedGuide, String> {
        if self.last_compiled.is_empty() {
            return Err(format!(
                "{operation}: no compiled revision; call compileProject first"
            ));
        }

        if let Some(entry) = &self.prepared_guide_cache {
            if entry.key == cache_key {
                #[cfg(test)]
                {
                    self.derived_cache_hits.prepared_guide += 1;
                }
                return Ok(entry.guide.clone());
            }
        }

        let snapshot_cache_hit = self
            .snapshot_completed_local_cache
            .as_ref()
            .is_some_and(|entry| entry.key == snapshot_cache_key);
        if snapshot_cache_hit {
            #[cfg(test)]
            {
                self.derived_cache_hits.snapshot_completed_local += 1;
            }
        } else {
            let entry = {
                let project = self.last_project.as_ref().ok_or_else(|| {
                    format!(
                        "{operation}: compileProject has not established a complete source revision"
                    )
                })?;
                let resolved = project.resolved_packages.as_ref().ok_or_else(|| {
                    format!("{operation}: compiled revision has no bound package resolver fixpoint")
                })?;
                let (_, fhir_version) = compiled_ig_identity(&self.last_compiled)?;
                // Validate the distinguished core, but snapshot against the complete
                // exact closure used by compileProject so profiles may derive from
                // external dependencies without consulting unrelated mounted versions.
                self.resolved_core_package(resolved, &project.config, &fhir_version, operation)?;
                let (source, cache_root, package_context) = self.source_for_resolved(resolved)?;
                let mut ctx =
                    snapshot_gen::PackageContext::new_with(source, &cache_root, &package_context)
                        .map_err(|e| format!("{operation}: package context: {e:#}"))?;
                // `last_compiled` is the complete local render set: generated FSH
                // resources, predefined `input/resources/**` bodies, and the explicitly
                // generated primary IG. Collapse it once by FHIR identity before any
                // semantic preparation. This preserves the old effective precedence
                // (later predefined inputs replace an identically keyed generated body),
                // while the explicit /__ig__ artifact remains authoritative for its own
                // identity.
                let (mut local_resources, primary_implementation_guide) =
                    prepared_local_resource_set(&self.last_compiled, operation)?;

                // Snapshot resolution must see the same complete, deduplicated local set
                // that PreparedGuide receives. In particular, predefined differential
                // profiles may derive from generated siblings or other predefined
                // profiles. PackageContext requires path order for disk-equivalent local
                // precedence, so do not rely on the render-set channel ordering here.
                let mut snapshot_locals = local_resources.clone();
                snapshot_locals.sort_by(|left, right| left.0.cmp(&right.0));
                ctx.load_local_resources(snapshot_locals);
                for (path, body) in &mut local_resources {
                    if body.get("resourceType").and_then(Value::as_str)
                        != Some("StructureDefinition")
                    {
                        continue;
                    }
                    let label = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("StructureDefinition");
                    *body = snapshot_gen::generate_snapshot(body.clone(), &ctx, Default::default())
                        .map_err(|e| format!("{operation}: snapshot {label}: {e:#}"))?;
                }
                let generated = local_resources
                    .into_iter()
                    .map(|(_, body)| body)
                    .collect::<Vec<_>>();
                SnapshotCompletedLocalCacheEntry {
                    key: snapshot_cache_key,
                    generated,
                    primary_implementation_guide,
                }
            };
            self.snapshot_completed_local_cache = Some(entry);
        }
        let snapshot = self
            .snapshot_completed_local_cache
            .as_ref()
            .expect("snapshot-completed local cache was hit or installed");
        let project = self.last_project.as_ref().ok_or_else(|| {
            format!("{operation}: compileProject has not established a complete source revision")
        })?;
        let guide = assemble_prepared_model(
            operation,
            &snapshot.generated,
            &snapshot.primary_implementation_guide,
            &project.config,
            &project.site_files,
            input.build_epoch_secs,
            &input.liquid_asset_dirs,
            input.branch.clone(),
            input.revision.clone(),
        )?;
        self.prepared_guide_cache = Some(PreparedGuideCacheEntry {
            key: cache_key,
            guide: guide.clone(),
        });
        Ok(guide)
    }

    fn site_build_project_revision(
        &self,
        project: &CompiledProjectRevision,
        project_id: &str,
    ) -> Result<site_build::ProjectRevision, String> {
        validate_predefined_site_overlap(&project.predefined, &project.site_files, "prepare")?;
        let mut entries: BTreeMap<site_build::SourcePath, site_build::SourceEntry> =
            BTreeMap::new();
        let mut insert = |path: &str,
                          kind: site_build::SourceKind,
                          bytes: &[u8],
                          media_type: &str|
         -> Result<(), String> {
            let path = site_build::SourcePath::parse(path.to_string())
                .map_err(|e| format!("prepare: source path {path}: {e}"))?;
            let entry = site_build::SourceEntry {
                kind,
                content: site_build::ContentRef::of_bytes(bytes, Some(media_type)),
            };
            if entries.insert(path.clone(), entry).is_some() {
                return Err(format!(
                    "prepare: source path {path} is declared by more than one input channel"
                ));
            }
            Ok(())
        };

        insert(
            "sushi-config.yaml",
            site_build::SourceKind::Config,
            project.config.as_bytes(),
            "application/yaml",
        )?;
        for (path, body) in &project.fsh {
            insert(
                path,
                site_build::SourceKind::Fsh,
                body.as_bytes(),
                "text/fhir-shorthand",
            )?;
        }
        for (path, body) in &project.predefined {
            // When the raw file is also in site_files, that exact authored byte
            // stream wins below. Otherwise preserve a deterministic normalized
            // resource representation rather than omitting this compile input.
            if project.site_files.contains_key(path) {
                continue;
            }
            let bytes = site_build::canonical_json_bytes(body)
                .map_err(|e| format!("prepare: canonical predefined {path}: {e}"))?;
            insert(
                path,
                site_build::SourceKind::PredefinedResource,
                &bytes,
                "application/fhir+json",
            )?;
        }
        for (path, encoded) in &project.site_files {
            let bytes = base64_decode(encoded)
                .map_err(|e| format!("prepare: bad base64 source {path}: {e}"))?;
            let (kind, media_type) = source_kind_and_media_type(path);
            insert(path, kind, &bytes, media_type)?;
        }

        let sources = site_build::SourceManifest::from_entries(entries)
            .map_err(|e| format!("prepare: source manifest: {e}"))?;
        let revision = format!(
            "sources-sha256:{}",
            site_build::sha256_canonical(&sources)
                .map_err(|e| format!("prepare: source revision: {e}"))?
        );
        Ok(site_build::ProjectRevision {
            project_id: project_id.to_string(),
            revision,
            sources,
        })
    }

    fn site_build_package_lock(
        &self,
        project: &CompiledProjectRevision,
    ) -> Result<site_build::PackageLock, String> {
        let resolved = project
            .resolved_packages
            .as_ref()
            .ok_or("prepare: compiled revision has no bound package resolver fixpoint")?;
        if resolved.config_sha256 != site_build::Sha256Digest::of_bytes(project.config.as_bytes()) {
            return Err(
                "prepare: compiled package closure belongs to different config bytes".into(),
            );
        }

        let mut coordinates_by_id: BTreeMap<String, Vec<site_build::PackageCoordinate>> =
            BTreeMap::new();
        let mut ordered = Vec::new();
        for label in &resolved.labels {
            let coordinate = site_build::PackageCoordinate::parse(label)
                .map_err(|e| format!("prepare: non-exact resolved package {label}: {e}"))?;
            coordinates_by_id
                .entry(coordinate.package_id().to_string())
                .or_default()
                .push(coordinate.clone());
            ordered.push((label, coordinate));
        }

        let mut support_by_id: BTreeMap<String, Vec<site_build::PackageCoordinate>> =
            BTreeMap::new();
        for label in &resolved.resolution_support {
            if !self.package_materials.contains_key(label) {
                return Err(format!(
                    "prepare: resolution-support package {label} has no mounted material"
                ));
            }
            let coordinate = site_build::PackageCoordinate::parse(label).map_err(|e| {
                format!("prepare: non-exact resolution-support package {label}: {e}")
            })?;
            support_by_id
                .entry(coordinate.package_id().to_string())
                .or_default()
                .push(coordinate);
        }

        let mut packages = Vec::new();
        for (label, coordinate) in ordered {
            let material = self.package_materials.get(label).ok_or_else(|| {
                format!("prepare: resolved package {label} has no mounted material")
            })?;
            let dependencies = material
                .declared_dependencies
                .iter()
                .filter_map(|(package_id, requested)| {
                    coordinates_by_id.get(package_id).map(|candidates| {
                        (
                            package_id,
                            requested,
                            candidates,
                            support_by_id
                                .get(package_id)
                                .map(Vec::as_slice)
                                .unwrap_or_default(),
                        )
                    })
                })
                .map(|(package_id, requested, candidates, support)| {
                    select_locked_dependency(package_id, requested, candidates, support)
                })
                .filter_map(Result::transpose)
                .collect::<Result<BTreeSet<_>, _>>()?;
            packages.push(site_build::LockedPackage {
                coordinate,
                content: material.content.clone(),
                dependencies,
            });
        }
        site_build::PackageLock::from_packages(packages)
            .map_err(|e| format!("prepare: package lock: {e}"))
    }

    /// Choose the single exact core package from the resolver fixpoint bound to
    /// these config bytes. Snapshotting must never consult an arbitrary mounted
    /// core that is absent from the SiteBuild package lock.
    fn resolved_core_package(
        &self,
        resolved: &ResolvedPackages,
        config: &str,
        fhir_version: &str,
        operation: &str,
    ) -> Result<String, String> {
        if resolved.config_sha256 != site_build::Sha256Digest::of_bytes(config.as_bytes()) {
            return Err(format!(
                "{operation}: resolved package closure belongs to different config bytes"
            ));
        }
        let (expected_id, expected_version) = core_coordinate_for_fhir_version(fhir_version)
            .map_err(|error| format!("{operation}: {error}"))?;
        let expected = format!("{expected_id}#{expected_version}");
        let matches: Vec<&String> = resolved
            .labels
            .iter()
            .filter(|label| label.split_once('#').map(|(id, _)| id) == Some(expected_id))
            .collect();
        match matches.as_slice() {
            [label] if label.as_str() == expected => {
                if !self.packages.iter().any(|mounted| mounted == *label)
                    || !self.package_materials.contains_key(*label)
                {
                    Err(format!(
                        "{operation}: resolved core package {label} has no mounted material"
                    ))
                } else {
                    Ok((*label).clone())
                }
            }
            [label] => Err(format!(
                "{operation}: FHIR version {fhir_version} requires {expected}, but the resolved closure contains {label}"
            )),
            [] => Err(format!(
                "{operation}: resolved closure has no required core package {expected}"
            )),
            _ => Err(format!(
                "{operation}: resolved closure contains multiple {expected_id} core packages"
            )),
        }
    }

    /// Resolve a project's two package sets against the CURRENTLY MOUNTED bundles.
    /// Returns the [`package_store::ResolutionStep`]'s canonical JSON STRING (the
    /// exact `ResolutionStep::to_json()` bytes the legacy wrapper hands back
    /// verbatim; the Session path re-parses it into the envelope).
    fn resolve_project(
        &mut self,
        config: &str,
        version_index_json: &str,
    ) -> Result<String, String> {
        let (source, cache_root, _packages) = self.source()?;

        let index: Option<package_store::VersionIndex> = if version_index_json.trim().is_empty() {
            None
        } else {
            Some(
                serde_json::from_str(version_index_json)
                    .map_err(|e| format!("resolve_project: bad version index JSON: {e}"))?,
            )
        };

        let step = package_store::resolve_project(config, &source, &cache_root, index.as_ref())
            .map_err(|e| format!("resolve_project: {e:#}"))?;
        self.resolved_packages = if step.satisfied {
            Some(resolved_packages_from_step(config, &step)?)
        } else {
            None
        };
        Ok(step.to_json())
    }
}

fn resolved_packages_from_step(
    config: &str,
    step: &package_store::ResolutionStep,
) -> Result<ResolvedPackages, String> {
    if !step.satisfied {
        return Err("package resolver step is not satisfied".into());
    }
    let mut labels = Vec::new();
    for request in step.compile_set.iter().chain(&step.context_closure) {
        let label = format!("{}#{}", request.package_id, request.version);
        if !labels.contains(&label) {
            labels.push(label);
        }
    }
    if labels.is_empty() {
        return Err("package resolver produced an empty satisfied closure".into());
    }
    let resolution_support = step
        .resolution_support
        .iter()
        .map(|request| format!("{}#{}", request.package_id, request.version))
        .collect();
    Ok(ResolvedPackages {
        config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
        resolution_support,
        labels,
    })
}

fn ordered_predefined_resources(resources: &BTreeMap<String, Value>) -> Vec<(PathBuf, Value)> {
    let mut ordered = resources
        .iter()
        .map(|(path, body)| (PathBuf::from(path), body.clone()))
        .collect::<Vec<_>>();
    ordered.sort_by(|(left, _), (right, _)| {
        compiler::predefined::input_path_rank(left)
            .cmp(&compiler::predefined::input_path_rank(right))
            .then_with(|| left.cmp(right))
    });
    ordered
}

fn predefined_render_path(source: &Path) -> Result<PathBuf, String> {
    let source_text = source
        .to_str()
        .ok_or_else(|| format!("compile: local resource path is not UTF-8: {source:?}"))?;
    let source_path = site_build::SourcePath::parse(source_text.to_string())
        .map_err(|error| format!("compile: invalid local resource path {source:?}: {error}"))?;
    Ok(PathBuf::from(format!(
        "/__predefined__/{}",
        source_path.as_str()
    )))
}

/// Select the one complete local resource input to PreparedGuide.
///
/// `compile_with_resolved` deliberately records channels in effective
/// precedence order (generated FSH, predefined resources, explicit generated
/// IG). The former `generated + examples` handoff happened to deduplicate by
/// `(resourceType, id)` inside semantic preparation with the later channel
/// winning. Make that rule explicit here so snapshot generation and semantic
/// preparation consume exactly the same bodies. The primary IG is selected by
/// its `/__ig__` ownership marker, never by position or resource identity, and
/// remains authoritative if another channel repeats its key.
fn prepared_local_resource_set(
    render_set: &[(PathBuf, Value)],
    operation: &str,
) -> Result<(Vec<(PathBuf, Value)>, Value), String> {
    let explicit = render_set
        .iter()
        .enumerate()
        .filter(|(_, (path, body))| {
            path.starts_with("/__ig__")
                && body.get("resourceType").and_then(Value::as_str) == Some("ImplementationGuide")
        })
        .collect::<Vec<_>>();
    let (primary_index, (primary_path, primary)) = match explicit.as_slice() {
        [(index, entry)] => (*index, *entry),
        [] => {
            return Err(format!(
                "{operation}: compiled revision has no explicit primary ImplementationGuide artifact"
            ));
        }
        _ => {
            return Err(format!(
                "{operation}: compiled revision has multiple explicit primary ImplementationGuide artifacts"
            ));
        }
    };

    let key = |body: &Value| -> Option<(String, String)> {
        Some((
            body.get("resourceType")?.as_str()?.to_string(),
            body.get("id")?.as_str()?.to_string(),
        ))
    };
    let primary_key = key(primary).ok_or_else(|| {
        format!("{operation}: explicit primary ImplementationGuide has no resourceType/id")
    })?;

    let mut selected: BTreeMap<(String, String), (usize, PathBuf, Value)> = BTreeMap::new();
    for (index, (path, body)) in render_set.iter().enumerate() {
        if let Some(key) = key(body) {
            selected.insert(key, (index, path.clone(), body.clone()));
        }
    }
    selected.insert(
        primary_key,
        (primary_index, primary_path.clone(), primary.clone()),
    );

    let mut resources = selected.into_values().collect::<Vec<_>>();
    resources.sort_by_key(|(index, _, _)| *index);
    Ok((
        resources
            .into_iter()
            .map(|(_, path, body)| (path, body))
            .collect(),
        primary.clone(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn assemble_prepared_model(
    operation: &str,
    generated: &[Value],
    primary_implementation_guide: &Value,
    config: &str,
    site_files: &BTreeMap<String, String>,
    build_epoch_secs: i64,
    liquid_asset_dirs: &[String],
    branch: Option<String>,
    revision: Option<String>,
) -> Result<site_build::PreparedGuide, String> {
    let ig_root = PathBuf::from("/ig");
    let mut vfs: BTreeMap<PathBuf, Vec<u8>> = BTreeMap::new();
    for (path, b64) in site_files {
        let bytes =
            base64_decode(b64).map_err(|e| format!("{operation}: bad base64 for {path}: {e}"))?;
        vfs.insert(ig_root.join(path), bytes);
    }
    let liquid_asset_rel_dirs = if liquid_asset_dirs.is_empty() {
        vec!["input/includes".to_string()]
    } else {
        liquid_asset_dirs.to_vec()
    };
    let files = prepared_guide::MemFiles::new(vfs);
    prepared_guide::semantics::prepare(&prepared_guide::semantics::PrepareInputs {
        generated,
        primary_implementation_guide,
        // Every local resource has already been selected, deduplicated, and
        // snapshot-completed above. Example-ness is publication metadata on the
        // explicit IG's definition.resource entries, not a second resource
        // transport channel.
        examples: &[],
        sushi_config_yaml: config,
        build_epoch_secs,
        branch,
        revision,
        augmentation: prepared_guide::AugmentInputs {
            ig: primary_implementation_guide,
            sushi_config_yaml: config,
            project_root: ig_root.clone(),
            pagecontent_dir: ig_root.join("input/pagecontent"),
            image_dir: ig_root.join("input/images"),
            liquid_asset_dirs: liquid_asset_rel_dirs
                .iter()
                .map(|directory| ig_root.join(directory))
                .collect(),
            files: &files,
        },
    })
    .map_err(|e| format!("{operation}: prepare guide: {e:#}"))
}

// ---------------------------------------------------------------------------
// JS-facing result shapes (serde -> JSON string the Worker JSON.parse()s, the
// simplest robust bindgen contract).
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
struct CompileResult {
    resources: Vec<CompiledResourceJs>,
    diagnostics: Vec<DiagnosticJs>,
    timings: Timings,
}

/// A SUSHI-exact diagnostic, shaped for the editor worker → Monaco markers.
/// `file`/`line` are present when the compiler had a source span in scope.
#[derive(Clone, Serialize)]
struct DiagnosticJs {
    severity: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<u32>,
}

#[derive(Clone, Serialize)]
struct CompiledResourceJs {
    filename: String,
    /// The exact bytes SUSHI writes (already FHIR-canonical JSON as a string).
    text: String,
    #[serde(rename = "resourceType")]
    resource_type: Option<String>,
    id: Option<String>,
    url: Option<String>,
    /// Exact authored declaration that produced this output. Generated
    /// resources have no declaration, so this key is omitted.
    #[serde(skip_serializing_if = "Option::is_none")]
    definition: Option<DefinitionJs>,
}

#[derive(Clone, Serialize, PartialEq, Eq, Debug)]
struct DefinitionJs {
    kind: &'static str,
    path: String,
    /// 1-based authored line.
    line: u32,
    /// 0-based authored column.
    column: u32,
}

impl From<compiler::CompiledResource> for CompiledResourceJs {
    fn from(resource: compiler::CompiledResource) -> Self {
        let compiler::CompiledResource {
            filename,
            text,
            body,
            definition,
        } = resource;
        let resource_type = body
            .get("resourceType")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let id = body
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let url = body
            .get("url")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let definition = definition.map(|definition| DefinitionJs {
            kind: match definition.kind {
                compiler::DefinitionKind::FshDeclaration => "fsh-declaration",
            },
            path: definition.path,
            line: definition.line,
            column: definition.column,
        });
        Self {
            filename,
            text,
            resource_type,
            id,
            url,
            definition,
        }
    }
}

#[derive(Clone, Serialize, Default)]
struct Timings {
    /// Milliseconds for the in-memory compile. Wall clock is unavailable under
    /// `wasm32-unknown-unknown` without JS help, so the Worker measures the call
    /// boundary; this field is populated by the caller-supplied timer when given,
    /// else 0. (See the demo Worker: it wraps calls in `performance.now()`.)
    compile_ms: f64,
}

#[derive(Serialize)]
struct SnapshotResult {
    snapshot: Option<Value>,
    messages: Vec<String>,
}

#[derive(Deserialize)]
struct BundleInput {
    label: String,
    files: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PackageMountResult {
    mounted: u32,
    added: u32,
    packages: u32,
    manifest_json_bytes: u64,
    artifact_bytes: u64,
    retained_blob_bytes: u64,
    indexed_members: u64,
    member_body_copies: u64,
    manifest_parse_ms: f64,
    decode_validate_ms: f64,
    mount_ms: f64,
    #[serde(flatten)]
    compression: package_store::BundleCompressionMetrics,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreparedStageResult {
    label: String,
    staged: u32,
    artifact_bytes: u64,
    indexed_members: u64,
    decode_validate_ms: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PreparedExport {
    label: String,
    cache_key: String,
    artifact_sha256: String,
    bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrepareMountResult {
    mounted: u32,
    added: u32,
    artifacts: Vec<PreparedExport>,
    artifact_bytes: u64,
    prepared_members: u64,
    input_json_bytes: u64,
    base64_bytes: u64,
    decoded_source_bytes: u64,
    normalized_bytes: u64,
    mount_member_body_copies: u64,
    json_parse_ms: f64,
    base64_decode_ms: f64,
    normalization_ms: f64,
    indexing_ms: f64,
    artifact_encode_ms: f64,
    /// Compatibility aggregate for hosts that consumed the initial API.
    decode_validate_prepare_ms: f64,
    mount_ms: f64,
}

/// Generator-only input to the exact preceding `compileProject` revision.
/// Authored project bytes are deliberately absent: they cross the boundary
/// once at compile and are captured by `last_project`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "generator", rename_all = "camelCase", deny_unknown_fields)]
enum GeneratorSpec {
    Cycle {
        #[serde(rename = "buildEpochSecs")]
        build_epoch_secs: i64,
        #[serde(default, rename = "liquidAssetDirs")]
        liquid_asset_dirs: Vec<String>,
        #[serde(default)]
        branch: Option<String>,
        #[serde(default)]
        revision: Option<String>,
    },
    Publisher {
        #[serde(rename = "templateCoordinate")]
        template_coordinate: String,
        #[serde(rename = "buildEpochSecs")]
        build_epoch_secs: i64,
        #[serde(default, rename = "activeTables")]
        active_tables: bool,
        #[serde(default, rename = "runUuid")]
        run_uuid: Option<String>,
    },
}

#[derive(Clone)]
struct PrepareGuideOptions {
    build_epoch_secs: i64,
    liquid_asset_dirs: Vec<String>,
    branch: Option<String>,
    revision: Option<String>,
}

const PREPARED_GUIDE_CACHE_SCHEMA: &str = "prepared-guide-cache-key/v1";
const PREPARED_GUIDE_RECIPE: &str = "sushi.snapshot+site-semantics/v1";
const SNAPSHOT_COMPLETED_LOCAL_CACHE_SCHEMA: &str = "snapshot-completed-local-cache-key/v1";
const SNAPSHOT_COMPLETED_LOCAL_RECIPE: &str = "snapshot-gen.walk+local-precedence/v1";
const CLOSED_SITE_BUILD_CACHE_SCHEMA: &str = "closed-site-build-cache-key/v1";
const CYCLE_PROJECTION_RECIPE: &str = "site-build.cycle-projection/v2";
const PUBLISHER_RUNTIME_PREPARATION_SCHEMA: &str = "publisher-runtime-preparation-key/v1";
// This private recipe covers the complete preparation implementation from an
// exact PreparedGuide through template/runtime assembly, ProducerInputs,
// RenderState, output catalog, and ClosedSiteBuild. A retained hit can only
// occur inside one immutable engine binary, but naming the recipe makes that
// boundary explicit and keeps invalidation reviewable.
const PUBLISHER_RUNTIME_PREPARATION_RECIPE: &str =
    "publisher-template-rust.runtime+model+render+catalog/v1";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PreparedGuideCachePayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    engine_api: u32,
    project: &'a site_build::ProjectRevision,
    package_lock: &'a site_build::PackageLock,
    compiled_sha256: &'a site_build::Sha256Digest,
    build_epoch_secs: i64,
    liquid_asset_dirs: &'a [String],
    branch: Option<&'a str>,
    revision: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SnapshotCompletedLocalCachePayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    engine_api: u32,
    compiled_sha256: &'a site_build::Sha256Digest,
    package_lock: &'a site_build::PackageLock,
    resolved_config_sha256: &'a site_build::Sha256Digest,
    resolution_support: &'a BTreeSet<String>,
    /// PackageContext precedence is ordered; PackageLock is deliberately
    /// coordinate-sorted, so the resolver's exact root-first order is also
    /// required cache identity.
    resolved_labels: &'a [String],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ClosedSiteBuildCachePayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    prepared_guide_key: &'a site_build::Sha256Digest,
    render_target: &'a site_build::RenderTarget,
    diagnostics: &'a BTreeSet<site_build::BuildDiagnostic>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublisherRuntimePreparationKeyPayload<'a> {
    schema: &'static str,
    recipe: &'a str,
    engine_api: u32,
    project: &'a site_build::ProjectRevision,
    package_lock: &'a site_build::PackageLock,
    prepared_guide_key: &'a site_build::Sha256Digest,
    diagnostics: &'a BTreeSet<site_build::BuildDiagnostic>,
    template: &'a site_build::PackageCoordinate,
    template_chain: &'a [String],
    renderer: &'a site_build::ProducerRef,
    active_tables: bool,
    run_uuid: Option<&'a str>,
}

fn publisher_runtime_preparation_key(
    payload: &PublisherRuntimePreparationKeyPayload<'_>,
) -> Result<site_build::Sha256Digest, site_build::CanonicalError> {
    site_build::sha256_canonical(payload)
}

fn closed_site_build_cache_key(
    prepared_guide_key: &site_build::Sha256Digest,
    render_target: &site_build::RenderTarget,
    diagnostics: &BTreeSet<site_build::BuildDiagnostic>,
) -> Result<site_build::Sha256Digest, site_build::CanonicalError> {
    site_build::sha256_canonical(&ClosedSiteBuildCachePayload {
        schema: CLOSED_SITE_BUILD_CACHE_SCHEMA,
        recipe: CYCLE_PROJECTION_RECIPE,
        prepared_guide_key,
        render_target,
        diagnostics,
    })
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrepareSiteResult {
    handle: String,
    build_id: String,
    generator: String,
    site_build: site_build::ClosedSiteBuild,
    metrics: PrepareMetrics,
}

/// Diagnostic timings for the existing `prepare` operation. These are an
/// observational sidecar, never part of SiteBuild identity or cache authority.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct PrepareMetrics {
    total_ms: f64,
    project_revision_ms: f64,
    package_lock_ms: f64,
    prepared_guide_key_ms: f64,
    prepared_guide_ms: f64,
    snapshot_completed_local_cache_hit: bool,
    prepared_guide_cache_hit: bool,
    site_build_cache_hit: bool,
    template_materialize_ms: f64,
    publisher_runtime_ms: f64,
    publisher_model_ms: f64,
    render_model_ms: f64,
    catalog_ms: f64,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputDescriptor {
    path: site_build::OutputPath,
    kind: &'static str,
    media_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<site_build::ContentRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject: Option<OutputResourceSubject>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subject_page: Option<OutputSubjectPage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputResourceSubject {
    resource_type: String,
    id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum OutputSubjectPage {
    Primary,
    Companion,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputCatalogResult {
    build_id: String,
    outputs: Vec<OutputDescriptor>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderSiteResult {
    path: site_build::OutputPath,
    media_type: String,
    content: site_build::ContentRef,
    non_ready_fragments: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExternalFinalizeInput {
    renderer: site_build::RendererImplementation,
    output_schema: String,
    #[serde(default)]
    options: BTreeMap<String, String>,
    catalog: Vec<site_build::OutputPath>,
    files: Vec<site_build::SiteOutputFile>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TemplateResolutionWire {
    satisfied: bool,
    chain: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    missing: Option<String>,
}

fn site_build_diagnostics(diagnostics: &[DiagnosticJs]) -> BTreeSet<site_build::BuildDiagnostic> {
    diagnostics
        .iter()
        .enumerate()
        .map(|(sequence, diagnostic)| {
            let severity = match diagnostic.severity.to_ascii_lowercase().as_str() {
                "error" => site_build::DiagnosticSeverity::Error,
                "warning" => site_build::DiagnosticSeverity::Warning,
                _ => site_build::DiagnosticSeverity::Information,
            };
            let mut out = site_build::BuildDiagnostic::new(
                severity,
                "sushi.compile",
                diagnostic.message.clone(),
            )
            .with_sequence(sequence as u64);
            if let (Some(file), Some(line)) = (&diagnostic.file, diagnostic.line) {
                if let Ok(path) = site_build::SourcePath::parse(file.clone()) {
                    out.location = Some(site_build::SourceLocation {
                        path,
                        line,
                        column: 0,
                    });
                }
            }
            out
        })
        .collect()
}

fn select_locked_dependency(
    package_id: &str,
    requested: &str,
    candidates: &[site_build::PackageCoordinate],
    resolution_support: &[site_build::PackageCoordinate],
) -> Result<Option<site_build::PackageCoordinate>, String> {
    let requested = if package_id.starts_with("hl7.fhir.")
        && package_id.ends_with(".core")
        && requested == "4.0.0"
    {
        "4.0.1"
    } else {
        requested
    };
    let mut matches = candidates
        .iter()
        .filter(|candidate| package_version_matches(candidate.version(), requested))
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| package_version_cmp(left.version(), right.version()));
    if let Some(selected) = matches.pop() {
        return Ok(Some(selected));
    }

    // The context resolver reads a dependency manifest before applying its R4
    // compatibility filter. A matching support coordinate proves that this
    // exact declared edge was intentionally excluded from the executable
    // closure. Do not retarget it to an unrelated resolved version and do not
    // place a dangling edge in PackageLock.
    if resolution_support
        .iter()
        .any(|candidate| package_version_matches(candidate.version(), requested))
    {
        return Ok(None);
    }

    Err(format!(
        "prepare: dependency {package_id}#{requested} matches neither the resolved coordinates nor an intentionally excluded resolution-support coordinate; resolved: {}; support: {}",
        candidates
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        resolution_support
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

fn package_version_matches(version: &str, pattern: &str) -> bool {
    if matches!(
        pattern.to_ascii_lowercase().as_str(),
        "latest" | "current" | "dev"
    ) {
        return true;
    }
    let pattern_parts = pattern.split('.').collect::<Vec<_>>();
    let version_parts = version.split('.').collect::<Vec<_>>();
    for (index, part) in pattern_parts.iter().enumerate() {
        if matches!(part.to_ascii_lowercase().as_str(), "x" | "*") {
            return true;
        }
        if version_parts.get(index) != Some(part) {
            return false;
        }
    }
    true
}

fn package_version_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let parts = |value: &str| {
        value
            .split(['.', '-'])
            .map(|part| {
                part.parse::<u64>()
                    .map(|number| (true, number, String::new()))
                    .unwrap_or_else(|_| (false, 0, part.to_string()))
            })
            .collect::<Vec<_>>()
    };
    let left = parts(left);
    let right = parts(right);
    for index in 0..left.len().max(right.len()) {
        match (left.get(index), right.get(index)) {
            (
                Some((left_numeric, left_number, left_text)),
                Some((right_numeric, right_number, right_text)),
            ) => {
                let ordering = left_numeric
                    .cmp(right_numeric)
                    .then_with(|| left_number.cmp(right_number))
                    .then_with(|| left_text.cmp(right_text));
                if ordering != std::cmp::Ordering::Equal {
                    return ordering;
                }
            }
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (None, None) => break,
        }
    }
    std::cmp::Ordering::Equal
}

// The result/error envelope helpers now live in the shared `api_envelope` crate
// (imported above) — one implementation for the Session and the `fig` CLI.
// ===========================================================================
// Session — the preferred isolated engine handle, with grouped methods and the
// uniform envelope.
// ===========================================================================

/// The editor's engine session. Construct once; call methods per operation.
///
/// Every `Session` owns its mutable compiler/package coordinator. Site handles
/// below it name immutable build runtimes and never an ambient latest site.
#[wasm_bindgen]
pub struct Session {
    engine: RefCell<Engine>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    fn with_engine<T>(&self, f: impl FnOnce(&mut Engine) -> T) -> T {
        f(&mut self.engine.borrow_mut())
    }
}

#[wasm_bindgen]
impl Session {
    /// Create an isolated session handle.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Session {
        set_panic_hook();
        Session {
            engine: RefCell::new(Engine::default()),
        }
    }

    /// Mount a set of prebuilt package bundles as the package cache, REPLACING any
    /// prior mount. `bundles_json`: `[{ "label": "id#ver", "files": { name: b64 }}]`.
    /// Envelope result: `{ "mounted": <count> }`.
    pub fn init(&self, bundles_json: &str) -> String {
        set_panic_hook();
        envelope(
            "init",
            self.with_engine(|e| e.init(bundles_json))
                .map(|n| serde_json::json!({ "mounted": n })),
        )
    }

    /// Mount ADDITIONAL bundles (additive, idempotent). Envelope result:
    /// `{ "mounted": <total-count> }`.
    pub fn mount(&self, bundles_json: &str) -> String {
        set_panic_hook();
        envelope(
            "mount",
            self.with_engine(|e| e.mount(bundles_json))
                .map(|n| serde_json::json!({ "mounted": n })),
        )
    }

    /// Mount one binary PreparedPackage without JSON/base64 transport or a
    /// derived-index rebuild. `expected_key` is the exact manifest/cache key.
    #[wasm_bindgen(js_name = mountPrepared)]
    pub fn mount_prepared(&self, bytes: Vec<u8>, expected_key: &str) -> String {
        set_panic_hook();
        envelope(
            "mountPrepared",
            self.with_engine(|engine| engine.mount_prepared(bytes, expected_key))
                .map(|mounted| serde_json::json!({ "mounted": mounted })),
        )
    }

    /// Begin an all-or-nothing compact prepared-package transaction. Artifacts
    /// are staged individually to bound host peak memory, then committed once.
    #[wasm_bindgen(js_name = beginPreparedMount)]
    pub fn begin_prepared_mount(&self, expected_packages: u32) -> String {
        set_panic_hook();
        envelope(
            "beginPreparedMount",
            self.with_engine(|engine| engine.begin_prepared_mount(expected_packages))
                .map(|()| serde_json::json!({ "expectedPackages": expected_packages })),
        )
    }

    #[wasm_bindgen(js_name = stagePreparedMount)]
    pub fn stage_prepared_mount(&self, bytes: Vec<u8>, expected_key: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "stagePreparedMount",
            self.with_engine(|engine| engine.stage_prepared_mount(bytes, expected_key)),
        )
    }

    #[wasm_bindgen(js_name = commitPreparedMount)]
    pub fn commit_prepared_mount(&self) -> String {
        set_panic_hook();
        envelope_ser(
            "commitPreparedMount",
            self.with_engine(Engine::commit_prepared_mount),
        )
    }

    #[wasm_bindgen(js_name = abortPreparedMount)]
    pub fn abort_prepared_mount(&self) -> String {
        set_panic_hook();
        envelope(
            "abortPreparedMount",
            self.with_engine(|engine| Ok(engine.abort_prepared_mount()))
                .map(|aborted| serde_json::json!({ "aborted": aborted })),
        )
    }

    /// Current compact-package retention and lazy-inflate counters. This is a
    /// read-only diagnostic surface; it is not part of package authority.
    #[wasm_bindgen(js_name = packageStorageMetrics)]
    pub fn package_storage_metrics(&self) -> String {
        set_panic_hook();
        envelope_ser(
            "packageStorageMetrics",
            self.with_engine(|engine| Ok::<_, String>(engine.package_storage_metrics())),
        )
    }

    /// Cold-path bridge for the current inflated JSON/base64 package shape.
    /// Normalizes each package once, mounts it transactionally, and stages the
    /// exact `.fpp` artifact for zero-base64 transfer through `takePrepared`.
    #[wasm_bindgen(js_name = prepareAndMount)]
    pub fn prepare_and_mount(&self, bundles_json: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "prepareAndMount",
            self.with_engine(|engine| engine.prepare_and_mount(bundles_json)),
        )
    }

    /// Move one artifact staged by `prepareAndMount` into a JS `Uint8Array`.
    /// Metadata and errors for preparation remain in the uniform JSON envelope;
    /// a missing/twice-taken binary is surfaced as a JS exception.
    #[wasm_bindgen(js_name = takePrepared)]
    pub fn take_prepared(&self, label: &str) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
        self.with_engine(|engine| engine.take_prepared(label))
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error))
    }

    /// Compile one complete project revision, including its authored site-file
    /// manifest. The latter supplies page-folder names to IG export so downstream
    /// SiteBuild projections reuse this exact compile instead of rerunning
    /// the compiler. Envelope result matches `compile()`.
    #[wasm_bindgen(js_name = compileProject)]
    pub fn compile_project(
        &self,
        files_json: &str,
        config: &str,
        predefined_json: &str,
        site_files_json: &str,
    ) -> String {
        set_panic_hook();
        envelope_ser(
            "compileProject",
            self.with_engine(|e| {
                e.compile_project(files_json, config, predefined_json, site_files_json)
            }),
        )
    }

    /// Generate a snapshot for an inline SD JSON or a canonical URL/id/name.
    /// Envelope result: `{ snapshot, messages }`.
    pub fn snapshot(&self, input: &str) -> String {
        set_panic_hook();
        envelope_ser("snapshot", self.with_engine(|e| e.snapshot(input)))
    }

    /// Prepare one generator against the exact project captured by the preceding
    /// `compileProject`. The specification contains generator choices only;
    /// authored project bytes are never accepted a second time.
    #[wasm_bindgen(js_name = prepare)]
    pub fn prepare_site(&self, generator_spec_json: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "prepare",
            self.with_engine(|engine| engine.prepare_site(generator_spec_json)),
        )
    }

    /// Complete, collision-checked output inventory for an immutable build.
    #[wasm_bindgen(js_name = outputs)]
    pub fn site_outputs(&self, handle: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "outputs",
            self.with_engine(|engine| engine.site_outputs(handle)),
        )
    }

    /// Materialize one declared output independently of every other path.
    #[wasm_bindgen(js_name = render)]
    pub fn render_site_output(&self, handle: &str, path: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "render",
            self.with_engine(|engine| engine.render_site_output(handle, path)),
        )
    }

    /// Return the canonical Rust SiteOutput after every catalog path is ready.
    #[wasm_bindgen(js_name = finalize)]
    pub fn finalize_site(&self, handle: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "finalize",
            self.with_engine(|engine| engine.finalize_site(handle)),
        )
    }

    /// Internal ContentStore bridge. Public worker operations return only
    /// ContentRefs; the worker drains this direct Uint8Array, verifies it, and
    /// publishes it to OPFS before resolving the public operation.
    #[wasm_bindgen(js_name = readContent)]
    pub fn read_site_content(
        &self,
        handle: &str,
        sha256: &str,
    ) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
        self.with_engine(|engine| engine.read_site_content(handle, sha256))
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error))
    }

    /// Internal external-renderer receipt authority. The worker's public Cycle
    /// `finalize(handle)` supplies only its already-verified ContentRefs and
    /// metadata; Rust checks catalog equality and emits canonical SiteOutput.
    #[wasm_bindgen(js_name = finalizeExternal)]
    pub fn finalize_external_site(&self, handle: &str, input_json: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "finalizeExternal",
            self.with_engine(|engine| engine.finalize_external_site(handle, input_json)),
        )
    }

    /// Tier-1 in-engine ValueSet expansion. Envelope result is the expansion
    /// payload (`{ ok, expansion, ... }` or `{ ok:false, notEnumerable }`).
    #[wasm_bindgen(js_name = expandValueSet)]
    pub fn expand_valueset(&self, valueset_json: &str, resources_json: &str) -> String {
        set_panic_hook();
        envelope(
            "expandValueSet",
            self.with_engine(|e| e.expand_valueset(valueset_json, resources_json)),
        )
    }

    /// Resolve a project's package sets against the mounted bundles. Envelope
    /// result: `{ resolver_schema, compile_set, context_closure,
    /// resolution_support, missing, satisfied, mutable_requests }`. Support
    /// packages are manifests needed to prove exclusions and must accompany a
    /// replayed closure, though they are not compile/render inputs.
    #[wasm_bindgen(js_name = resolveProject)]
    pub fn resolve_project(&self, config: &str, version_index_json: &str) -> String {
        set_panic_hook();
        let payload = self
            .with_engine(|e| e.resolve_project(config, version_index_json))
            .and_then(|s| {
                serde_json::from_str::<Value>(&s)
                    .map_err(|e| format!("resolveProject: reparse: {e}"))
            });
        envelope("resolveProject", payload)
    }

    /// Private package-acquisition handshake for Publisher templates. Rust is
    /// the sole interpreter of `package.json.base`: the host fetches the one
    /// exact `missing` coordinate through ordinary package plumbing, mounts it,
    /// and retries until `satisfied`.
    #[wasm_bindgen(js_name = resolveTemplate)]
    pub fn resolve_template(&self, coordinate: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "resolveTemplate",
            self.with_engine(|engine| engine.resolve_template(coordinate)),
        )
    }

    /// Engine version + build commit, as a JSON string `{ version, commit, engine }`
    /// (NOT enveloped — a static build-info accessor).
    pub fn version() -> String {
        version_json()
    }
}

// ===========================================================================
// Shared helpers (used by the Engine methods above).
// ===========================================================================

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

fn version_json() -> String {
    let v = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("WASM_API_GIT_COMMIT").unwrap_or("unknown"),
        "engine": "rust_sushi + snapshot_gen (walk)",
        "apiVersion": API_VERSION,
    });
    v.to_string()
}

/// Standard-alphabet base64 with `=` padding for the generic in-memory CAS
/// transport. Kept dependency-free beside the matching decoder below.
#[cfg(test)]
fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0] as u32;
        let second = *chunk.get(1).unwrap_or(&0) as u32;
        let third = *chunk.get(2).unwrap_or(&0) as u32;
        let value = (first << 16) | (second << 8) | third;
        output.push(ALPHABET[((value >> 18) & 63) as usize] as char);
        output.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

/// Decode + mount each bundle's base64 files under its label. Appends newly
/// mounted labels to `labels`.
fn mount_into(
    src: &mut BundleSource,
    parsed: &[BundleInput],
    labels: &mut Vec<String>,
    package_materials: &mut BTreeMap<String, MountedPackage>,
    who: &str,
) -> Result<(), String> {
    for pkg in parsed {
        if labels.iter().any(|label| label == &pkg.label)
            || package_materials.contains_key(&pkg.label)
        {
            return Err(format!(
                "{who}: duplicate package label in one mount transaction: {}",
                pkg.label
            ));
        }
        let mut entries = BTreeMap::new();
        for (name, b64) in &pkg.files {
            let bytes =
                base64_decode(b64).map_err(|e| format!("{who}: bad base64 for {name}: {e}"))?;
            entries.insert(name.clone(), bytes);
        }
        let material = package_store::normalize_package_material(&pkg.label, entries)
            .map_err(|error| format!("{who}: invalid package {}: {error:#}", pkg.label))?;
        src.mount_package(&pkg.label, material.files);
        labels.push(pkg.label.clone());
        package_materials.insert(
            pkg.label.clone(),
            MountedPackage {
                content: site_build::ContentRef::of_bytes(
                    &material.payload,
                    Some(package_store::NORMALIZED_PACKAGE_MEDIA_TYPE),
                ),
                declared_dependencies: material.declared_dependencies,
            },
        );
    }
    Ok(())
}

fn prepared_content(
    package: &package_store::PreparedPackage,
    operation: &str,
) -> Result<site_build::ContentRef, String> {
    Ok(site_build::ContentRef {
        sha256: site_build::Sha256Digest::parse(&package.semantic_payload_sha256)
            .map_err(|error| format!("{operation}: invalid semantic digest: {error}"))?,
        byte_length: package.semantic_payload_bytes,
        media_type: Some(package_store::NORMALIZED_PACKAGE_MEDIA_TYPE.to_string()),
    })
}

fn compression_delta(
    before: package_store::BundleCompressionMetrics,
    after: package_store::BundleCompressionMetrics,
) -> package_store::BundleCompressionMetrics {
    package_store::BundleCompressionMetrics {
        compressed_retained_bytes: after
            .compressed_retained_bytes
            .saturating_sub(before.compressed_retained_bytes),
        declared_raw_bytes: after
            .declared_raw_bytes
            .saturating_sub(before.declared_raw_bytes),
        chunks_inflated: after.chunks_inflated.saturating_sub(before.chunks_inflated),
        raw_inflated_bytes: after
            .raw_inflated_bytes
            .saturating_sub(before.raw_inflated_bytes),
        cache_hits: after.cache_hits.saturating_sub(before.cache_hits),
        cached_raw_bytes: after
            .cached_raw_bytes
            .saturating_sub(before.cached_raw_bytes),
    }
}

#[cfg(target_family = "wasm")]
fn clock_ms() -> f64 {
    date_now()
}

#[cfg(target_family = "wasm")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = Date, js_name = now)]
    fn date_now() -> f64;
}

#[cfg(not(target_family = "wasm"))]
fn clock_ms() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

fn is_local_resource_json(path: &str) -> bool {
    path.ends_with(".json")
        && (path.starts_with("input/resources/") || path.starts_with("input/examples/"))
}

fn validate_predefined_site_overlap(
    predefined: &BTreeMap<String, Value>,
    site_files: &BTreeMap<String, String>,
    operation: &str,
) -> Result<(), String> {
    let predefined_paths: BTreeSet<&str> = predefined.keys().map(String::as_str).collect();
    let raw_paths: BTreeSet<&str> = site_files
        .keys()
        .filter(|path| is_local_resource_json(path))
        .map(String::as_str)
        .collect();
    if predefined_paths != raw_paths {
        let only_predefined = predefined_paths
            .difference(&raw_paths)
            .copied()
            .collect::<Vec<_>>();
        let only_raw = raw_paths
            .difference(&predefined_paths)
            .copied()
            .collect::<Vec<_>>();
        return Err(format!(
            "{operation}: parsed and raw local-resource JSON paths differ (only parsed: {only_predefined:?}; only raw: {only_raw:?})"
        ));
    }
    for (path, compiled_value) in predefined {
        let encoded = site_files
            .get(path)
            .expect("equal resource path sets were checked above");
        let bytes = base64_decode(encoded)
            .map_err(|error| format!("{operation}: invalid base64 in {path}: {error}"))?;
        let authored_value = serde_json::from_slice::<Value>(&bytes)
            .map_err(|error| format!("{operation}: invalid JSON in {path}: {error}"))?;
        if &authored_value != compiled_value {
            return Err(format!(
                "{operation}: predefined resource {path} differs from the raw authored site file at the same path"
            ));
        }
    }
    Ok(())
}

fn source_kind_and_media_type(path: &str) -> (site_build::SourceKind, &'static str) {
    let lower = path.to_ascii_lowercase();
    let media_type = if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".yaml") || lower.ends_with(".yml") {
        "application/yaml"
    } else if lower.ends_with(".md") {
        "text/markdown"
    } else if lower.ends_with(".html") || lower.ends_with(".xhtml") {
        "text/html"
    } else if lower.ends_with(".css") {
        "text/css"
    } else if lower.ends_with(".js") {
        "text/javascript"
    } else if lower.ends_with(".svg") {
        "image/svg+xml"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else {
        "application/octet-stream"
    };
    let kind = if is_local_resource_json(path) {
        site_build::SourceKind::PredefinedResource
    } else if ["input/pagecontent/", "input/pages/", "input/resource-docs/"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        site_build::SourceKind::Page
    } else if path.starts_with("input/images/")
        || matches!(
            path.rsplit_once('.')
                .map(|(_, ext)| ext.to_ascii_lowercase())
                .as_deref(),
            Some(
                "css"
                    | "js"
                    | "png"
                    | "gif"
                    | "jpg"
                    | "jpeg"
                    | "svg"
                    | "ico"
                    | "woff"
                    | "woff2"
                    | "ttf"
            )
        )
    {
        site_build::SourceKind::Asset
    } else {
        site_build::SourceKind::Other {
            name: "site_file".into(),
        }
    };
    (kind, media_type)
}

fn compiled_ig_identity(compiled: &[(PathBuf, Value)]) -> Result<(String, String), String> {
    let ig = compiled
        .iter()
        .find(|(path, body)| {
            path.starts_with("/__ig__")
                && body.get("resourceType").and_then(Value::as_str) == Some("ImplementationGuide")
        })
        .map(|(_, body)| body)
        .ok_or("prepare: compiled revision has no ImplementationGuide")?;
    let project_id = ig
        .get("packageId")
        .or_else(|| ig.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or("prepare: ImplementationGuide has no packageId/id")?
        .to_string();
    let fhir_version = match ig.get("fhirVersion") {
        Some(Value::String(value)) => Some(value.as_str()),
        Some(Value::Array(values)) => values.first().and_then(Value::as_str),
        _ => None,
    }
    .filter(|value| !value.trim().is_empty())
    .ok_or("prepare: ImplementationGuide has no fhirVersion")?
    .to_string();
    Ok((project_id, fhir_version))
}

fn page_listing_from_site_files(
    site_files: &std::collections::BTreeMap<String, String>,
) -> std::collections::HashMap<String, Vec<String>> {
    let mut listing: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for path in site_files.keys() {
        for folder in ["pagecontent", "pages", "resource-docs"] {
            let prefix = format!("input/{folder}/");
            if let Some(rest) = path.strip_prefix(&prefix) {
                if !rest.is_empty() && !rest.contains('/') {
                    listing
                        .entry(folder.to_string())
                        .or_default()
                        .push(rest.to_string());
                }
            }
        }
    }
    for names in listing.values_mut() {
        names.sort();
        names.dedup();
    }
    listing
}
fn core_coordinate_for_fhir_version(fhir_version: &str) -> Result<(&'static str, String), String> {
    let numeric = fhir_version.split('-').next().unwrap_or(fhir_version);
    let mut parts = numeric.split('.');
    let major = parts.next().unwrap_or("");
    let minor = parts.next().unwrap_or("");
    let package_id = match (major, minor) {
        ("4", "0") => "hl7.fhir.r4.core",
        ("4", "1" | "3") => "hl7.fhir.r4b.core",
        ("5", _) => "hl7.fhir.r5.core",
        ("6", _) => "hl7.fhir.r6.core",
        _ => return Err(format!("unsupported FHIR version {fhir_version}")),
    };
    let version = if package_id == "hl7.fhir.r4.core" && fhir_version == "4.0.0" {
        "4.0.1".to_string()
    } else {
        fhir_version.to_string()
    };
    Ok((package_id, version))
}

fn target_core_from_package_lock(
    package_lock: &site_build::PackageLock,
    fhir_version: &str,
    operation: &str,
) -> Result<site_build::PackageCoordinate, String> {
    let (expected_id, expected_version) = core_coordinate_for_fhir_version(fhir_version)
        .map_err(|error| format!("{operation}: {error}"))?;
    let expected = format!("{expected_id}#{expected_version}");
    let matches = package_lock
        .iter()
        .map(|(coordinate, _)| coordinate)
        .filter(|coordinate| coordinate.package_id() == expected_id)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [coordinate] if coordinate.version() == expected_version => Ok((*coordinate).clone()),
        [coordinate] => Err(format!(
            "{operation}: FHIR version {fhir_version} requires target core {expected}, but the exact package lock contains {coordinate}"
        )),
        [] => Err(format!(
            "{operation}: exact package lock has no required target core {expected}"
        )),
        _ => Err(format!(
            "{operation}: exact package lock contains multiple coordinates for target core package {expected_id}"
        )),
    }
}

fn with_template_chain(
    compile_lock: &site_build::PackageLock,
    chain: &[String],
    materials: &BTreeMap<String, MountedPackage>,
    operation: &str,
) -> Result<site_build::PackageLock, String> {
    let mut packages = compile_lock
        .iter()
        .map(|(coordinate, package)| (coordinate.clone(), package.clone()))
        .collect::<BTreeMap<_, _>>();
    let coordinates = chain
        .iter()
        .map(|label| {
            site_build::PackageCoordinate::parse(label)
                .map(|coordinate| (label, coordinate))
                .map_err(|error| {
                    format!("{operation}: non-exact template package {label}: {error}")
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut candidates_by_id: BTreeMap<String, Vec<site_build::PackageCoordinate>> =
        BTreeMap::new();
    for coordinate in compile_lock
        .iter()
        .map(|(coordinate, _)| coordinate)
        .chain(coordinates.iter().map(|(_, coordinate)| coordinate))
    {
        candidates_by_id
            .entry(coordinate.package_id().to_string())
            .or_default()
            .push(coordinate.clone());
    }
    for (index, (label, coordinate)) in coordinates.iter().enumerate() {
        let material = materials.get(*label).ok_or_else(|| {
            format!("{operation}: template package {label} has no authenticated mounted material")
        })?;
        let mut dependencies = material
            .declared_dependencies
            .iter()
            .filter_map(|(package_id, requested)| {
                candidates_by_id
                    .get(package_id)
                    .map(|candidates| (package_id, requested, candidates))
            })
            .map(|(package_id, requested, candidates)| {
                select_locked_dependency(package_id, requested, candidates, &[]).and_then(
                    |selected| {
                        selected.ok_or_else(|| {
                            format!(
                                "{operation}: template dependency {package_id}#{requested} was unexpectedly excluded"
                            )
                        })
                    },
                )
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        if index > 0 {
            dependencies.insert(coordinates[index - 1].1.clone());
        }
        let mut locked = site_build::LockedPackage {
            coordinate: coordinate.clone(),
            content: material.content.clone(),
            dependencies,
        };
        if let Some(existing) = packages.get(coordinate) {
            if existing.content != locked.content {
                return Err(format!(
                    "{operation}: template coordinate {coordinate} disagrees with compile-lock material"
                ));
            }
            locked
                .dependencies
                .extend(existing.dependencies.iter().cloned());
        }
        packages.insert(coordinate.clone(), locked);
    }
    site_build::PackageLock::from_packages(packages.into_values())
        .map_err(|error| format!("{operation}: extend template package lock: {error}"))
}

const PAGE_MD_SHIM: &str = "---\r\n---\r\n{% include template-page-md.html %}";
const FRAGMENT_SUFFIXES: &[&str] = &[
    "intro",
    "introduction",
    "notes",
    "search",
    "summary",
    "examples",
];

fn is_fragment_include(name: &str) -> bool {
    FRAGMENT_SUFFIXES
        .iter()
        .any(|suffix| name.ends_with(&format!("-{suffix}")))
}

fn is_static_asset(path: &str) -> bool {
    matches!(
        path.rsplit_once('.')
            .map(|(_, extension)| extension.to_ascii_lowercase())
            .as_deref(),
        Some(
            "css"
                | "js"
                | "png"
                | "svg"
                | "jpg"
                | "jpeg"
                | "gif"
                | "webp"
                | "ico"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
                | "eot"
        )
    )
}

#[derive(Clone, Copy)]
enum PreparedOutputCollision {
    Reject,
    /// The normative Publisher namespace gives authenticated authored files
    /// precedence over renderer/runtime bytes at the same public path.
    AuthoredOverridesRenderer,
}

fn insert_prepared_output(
    outputs: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
    objects: &mut BTreeMap<site_build::Sha256Digest, Vec<u8>>,
    path: &str,
    bytes: Vec<u8>,
    media_type: &str,
    producer: site_build::OutputProducer,
    source: Option<String>,
    collision: PreparedOutputCollision,
) -> Result<(), String> {
    let path = site_build::OutputPath::parse(path.to_string())
        .map_err(|error| format!("invalid output path {path}: {error}"))?;
    if let Some(existing) = outputs.get(&path) {
        if matches!(collision, PreparedOutputCollision::Reject) {
            return Err(format!(
                "prepared output collision at {path}: producer {} and {}",
                existing.producer.id, producer.id
            ));
        }
    }
    let content = site_build::ContentRef::of_bytes(&bytes, Some(media_type));
    if let Some(existing) = objects.get(&content.sha256) {
        if existing != &bytes {
            return Err(format!("content digest collision for {path}"));
        }
    } else {
        objects.insert(content.sha256.clone(), bytes);
    }
    outputs.insert(
        path,
        PreparedOutput {
            content,
            producer,
            source,
            owner: None,
        },
    );
    Ok(())
}

fn add_page_relative_output_aliases(
    outputs: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
    pages: &[String],
) -> Result<(), String> {
    let directories = pages
        .iter()
        .filter_map(|page| page.rsplit_once('/').map(|(directory, _)| directory))
        .filter(|directory| !directory.is_empty())
        .collect::<BTreeSet<_>>();
    let originals = outputs
        .iter()
        .filter(|(path, _)| is_static_asset(path.as_str()))
        .map(|(path, output)| (path.clone(), output.clone()))
        .collect::<Vec<_>>();
    for directory in directories {
        for (path, output) in &originals {
            if path.as_str().starts_with(&format!("{directory}/")) {
                continue;
            }
            let alias = site_build::OutputPath::parse(format!("{directory}/{path}"))
                .map_err(|error| format!("prepare(publisher): invalid asset alias: {error}"))?;
            let mut alias_output = output.clone();
            alias_output.source = alias_output
                .source
                .as_ref()
                .map(|source| format!("{source}; alias={alias}"));
            // A directly declared output owns its path. Relative aliases are a
            // compatibility view only and never replace canonical/authored
            // content already present there.
            outputs.entry(alias).or_insert(alias_output);
        }
    }
    Ok(())
}

fn authored_projection_targets(file: &site_build::AuthoredFile) -> Result<Vec<String>, String> {
    let path = file.path.as_str();
    let mut targets = Vec::new();
    let tree = |path: String| format!("tree:/site/{path}");
    let output = |path: &str| -> Result<String, String> {
        site_build::OutputPath::parse(path.to_string())
            .map(|path| format!("output:{path}"))
            .map_err(|error| {
                format!(
                    "prepare(publisher): authored {:?} path {path} is not a safe output path: {error}",
                    file.role
                )
            })
    };
    match file.role {
        site_build::AuthoredFileRole::Image => targets.push(output(path)?),
        site_build::AuthoredFileRole::Data => targets.push(tree(format!("_data/{path}"))),
        site_build::AuthoredFileRole::Include | site_build::AuthoredFileRole::ResourceContent => {
            targets.push(tree(format!("_includes/{path}")));
        }
        site_build::AuthoredFileRole::PageContent => {
            if let Some(name) = path.strip_suffix(".md") {
                targets.push(tree(format!("_includes/en/{path}")));
                targets.push(tree(format!("_includes/{path}")));
                if !is_fragment_include(name) {
                    let page = format!("en/{name}.html");
                    targets.push(tree(page.clone()));
                    targets.push(output(&page)?);
                }
            } else if path.ends_with(".html") {
                let page = format!("en/{path}");
                targets.push(tree(page.clone()));
                targets.push(output(&page)?);
            }
        }
        site_build::AuthoredFileRole::ImageSource => {}
    }
    Ok(targets)
}

fn validate_authored_projections(files: &[site_build::AuthoredFile]) -> Result<(), String> {
    let mut claims = BTreeMap::<String, String>::new();
    for file in files {
        let owner = format!("{:?} {}", file.role, file.path);
        for target in authored_projection_targets(file)? {
            if let Some(existing) = claims.insert(target.clone(), owner.clone()) {
                return Err(format!(
                    "prepare(publisher): authored projection collision at {target}: {existing} and {owner}"
                ));
            }
        }
    }
    Ok(())
}

fn stage_prepared_authored_files(
    prepared: &site_build::PreparedGuide,
    site_files: &mut std::collections::HashMap<PathBuf, Vec<u8>>,
    outputs: &mut BTreeMap<site_build::OutputPath, PreparedOutput>,
    objects: &mut BTreeMap<site_build::Sha256Digest, Vec<u8>>,
    project_id: &str,
) -> Result<(), String> {
    // Validate the complete authored projection before mutating either tree.
    // Different semantic roles can map to the same Publisher location (for
    // example ResourceContent and Include both target `_includes/<path>`).
    // Traversal order must never decide which authored bytes survive.
    validate_authored_projections(&prepared.authored_files)?;

    for file in &prepared.authored_files {
        let path = file.path.as_str();
        let source = file
            .source_reads
            .iter()
            .map(|source| source.as_str())
            .collect::<Vec<_>>()
            .join(",");
        match file.role {
            site_build::AuthoredFileRole::Image => insert_prepared_output(
                outputs,
                objects,
                path,
                file.content.clone(),
                &file.mime,
                site_build::OutputProducer {
                    id: "publisher-authored-asset".into(),
                    version: project_id.into(),
                },
                Some(source),
                PreparedOutputCollision::AuthoredOverridesRenderer,
            )?,
            site_build::AuthoredFileRole::Data => {
                // Authored files are the documented final overlay over selected
                // template and generated Publisher model bytes.
                site_files.insert(
                    PathBuf::from(format!("/site/_data/{path}")),
                    file.content.clone(),
                );
            }
            site_build::AuthoredFileRole::Include
            | site_build::AuthoredFileRole::ResourceContent => {
                site_files.insert(
                    PathBuf::from(format!("/site/_includes/{path}")),
                    file.content.clone(),
                );
            }
            site_build::AuthoredFileRole::PageContent => {
                let Some(name) = path.strip_suffix(".md") else {
                    if path.ends_with(".html") {
                        site_files.insert(
                            PathBuf::from(format!("/site/en/{path}")),
                            file.content.clone(),
                        );
                    }
                    continue;
                };
                site_files.insert(
                    PathBuf::from(format!("/site/_includes/en/{path}")),
                    file.content.clone(),
                );
                site_files.insert(
                    PathBuf::from(format!("/site/_includes/{path}")),
                    file.content.clone(),
                );
                if !is_fragment_include(name) {
                    site_files.insert(
                        PathBuf::from(format!("/site/en/{name}.html")),
                        PAGE_MD_SHIM.as_bytes().to_vec(),
                    );
                }
            }
            site_build::AuthoredFileRole::ImageSource => {}
        }
    }
    Ok(())
}

fn prepared_render_set(
    prepared: &site_build::PreparedGuide,
) -> Result<Vec<(PathBuf, Value)>, String> {
    let mut seen = BTreeSet::new();
    prepared
        .resources
        .iter()
        .map(|resource| {
            if !seen.insert(resource.key.clone()) {
                return Err(format!(
                    "prepare(publisher): PreparedGuide repeats resource {}/{}",
                    resource.key.resource_type, resource.key.id
                ));
            }
            if resource.resource.get("resourceType").and_then(Value::as_str)
                != Some(resource.key.resource_type.as_str())
                || resource.resource.get("id").and_then(Value::as_str)
                    != Some(resource.key.id.as_str())
            {
                return Err(format!(
                    "prepare(publisher): PreparedGuide resource {}/{} disagrees with its JSON identity",
                    resource.key.resource_type, resource.key.id
                ));
            }
            let channel = if resource.key == prepared.guide.implementation_guide {
                "__ig__"
            } else {
                "__prepared__"
            };
            Ok((
                PathBuf::from(format!(
                    "/{channel}/{}-{}.json",
                    resource.key.resource_type, resource.key.id
                )),
                resource.resource.clone(),
            ))
        })
        .collect()
}

// A tiny dependency-free base64 decoder (standard alphabet, optional '='
// padding). Package bundle bytes arrive base64'd from JS; we avoid pulling a
// base64 crate for this one use.
fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<u8, String> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(format!("invalid base64 char: {c}")),
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    let mut chunk = bytes.chunks(4).peekable();
    while let Some(c) = chunk.next() {
        let n0 = val(c[0])?;
        let n1 = if c.len() > 1 { val(c[1])? } else { 0 };
        out.push((n0 << 2) | (n1 >> 4));
        if c.len() > 2 && c[2] != b'=' {
            let n2 = val(c[2])?;
            out.push((n1 << 4) | (n2 >> 2));
            if c.len() > 3 && c[3] != b'=' {
                let n3 = val(c[3])?;
                out.push((n2 << 6) | n3);
            }
        }
    }
    Ok(out)
}

// ===========================================================================
// F6 render surface — Engine methods (Session wrappers below in Session impl).
// ===========================================================================
impl Engine {
    fn resolve_template(&self, coordinate: &str) -> Result<TemplateResolutionWire, String> {
        if self.bundle.is_none() {
            return Ok(TemplateResolutionWire {
                satisfied: false,
                chain: Vec::new(),
                missing: Some(coordinate.to_string()),
            });
        }
        let (source, cache_root, _) = self.source()?;
        let resolution = package_store::resolve_template_base_chain(
            &source,
            &package_store::TemplatePaths::new(cache_root),
            coordinate,
        )
        .map_err(|error| format!("resolveTemplate: {error}"))?;
        Ok(TemplateResolutionWire {
            satisfied: resolution.satisfied(),
            chain: resolution.chain,
            missing: resolution.missing,
        })
    }

    /// Publish one fully prepared immutable runtime, then retire generations
    /// older than the immediately previous build. Call this only after every
    /// fallible preparation step has succeeded so an error cannot evict the
    /// current working generation.
    fn retain_site_build(&mut self, handle: String, runtime: SiteBuildRuntime) {
        self.site_build_generations
            .retain(|existing| existing != &handle);
        self.site_builds.insert(handle.clone(), runtime);
        self.site_build_generations.push_back(handle);
        while self.site_build_generations.len() > RETAINED_SITE_BUILD_LIMIT {
            let retired = self
                .site_build_generations
                .pop_front()
                .expect("retained generation limit exceeded");
            self.site_builds.remove(&retired);
        }
        debug_assert_eq!(self.site_builds.len(), self.site_build_generations.len());
        debug_assert!(self.site_builds.len() <= RETAINED_SITE_BUILD_LIMIT);
    }

    /// Reuse an exact Publisher preparation only while its complete runtime is
    /// already one of the two retained handles. A hit refreshes preparation
    /// recency but deliberately leaves the runtime in place, preserving its
    /// memoized rendered pages and object store.
    fn reuse_retained_publisher(
        &mut self,
        preparation_key: &site_build::Sha256Digest,
    ) -> Option<(String, site_build::ClosedSiteBuild)> {
        let (handle, build) =
            self.site_build_generations
                .iter()
                .rev()
                .find_map(|handle| match self.site_builds.get(handle) {
                    Some(SiteBuildRuntime::Publisher(runtime))
                        if &runtime.preparation_key == preparation_key =>
                    {
                        Some((handle.clone(), runtime.build.clone()))
                    }
                    _ => None,
                })?;
        self.site_build_generations
            .retain(|existing| existing != &handle);
        self.site_build_generations.push_back(handle.clone());
        #[cfg(test)]
        {
            self.derived_cache_hits.retained_publisher_runtime += 1;
        }
        Some((handle, build))
    }

    fn prepare_site(&mut self, spec_json: &str) -> Result<PrepareSiteResult, String> {
        let spec: GeneratorSpec = serde_json::from_str(spec_json)
            .map_err(|error| format!("prepare: invalid generator specification: {error}"))?;
        match spec {
            GeneratorSpec::Cycle {
                build_epoch_secs,
                liquid_asset_dirs,
                branch,
                revision,
            } => {
                let (projection, metrics) = self.prepare_cycle(&PrepareGuideOptions {
                    build_epoch_secs,
                    liquid_asset_dirs,
                    branch,
                    revision,
                })?;
                let handle = projection.site_build.site_build().build_id().to_string();
                self.retain_site_build(
                    handle.clone(),
                    SiteBuildRuntime::Cycle(CycleBuildRuntime {
                        build: projection.site_build.clone(),
                        objects: projection.objects,
                    }),
                );
                Ok(PrepareSiteResult {
                    handle: handle.clone(),
                    build_id: handle,
                    generator: "cycle".into(),
                    site_build: projection.site_build,
                    metrics,
                })
            }
            GeneratorSpec::Publisher {
                template_coordinate,
                build_epoch_secs,
                active_tables,
                run_uuid,
            } => self.prepare_publisher(
                &template_coordinate,
                PrepareGuideOptions {
                    build_epoch_secs,
                    liquid_asset_dirs: vec!["input/includes".into()],
                    branch: None,
                    revision: None,
                },
                active_tables,
                run_uuid,
            ),
        }
    }

    fn prepare_publisher(
        &mut self,
        template_coordinate: &str,
        guide_options: PrepareGuideOptions,
        active_tables: bool,
        run_uuid: Option<String>,
    ) -> Result<PrepareSiteResult, String> {
        let total_started = clock_ms();
        let operation = "prepare(publisher)";
        let mut metrics = PrepareMetrics::default();
        if template_coordinate.trim().is_empty() {
            return Err(format!("{operation}: template coordinate is empty"));
        }
        let template = site_build::PackageCoordinate::parse(template_coordinate)
            .map_err(|error| format!("{operation}: template coordinate must be exact: {error}"))?;
        let project = self.last_project.clone().ok_or_else(|| {
            format!("{operation}: compileProject has not established a complete source revision")
        })?;
        let (project_id, fhir_version) = compiled_ig_identity(&self.last_compiled)
            .map_err(|error| format!("{operation}: {error}"))?;
        let started = clock_ms();
        let project_revision = self.site_build_project_revision(&project, &project_id)?;
        metrics.project_revision_ms = (clock_ms() - started).max(0.0);
        let started = clock_ms();
        let package_lock = self.site_build_package_lock(&project)?;
        metrics.package_lock_ms = (clock_ms() - started).max(0.0);
        let started = clock_ms();
        let resolved = project.resolved_packages.as_ref().ok_or_else(|| {
            format!("{operation}: compiled revision has no bound package resolver fixpoint")
        })?;
        let cache_keys = self.preparation_cache_keys(
            &guide_options,
            &project_revision,
            &package_lock,
            resolved,
            operation,
        )?;
        let prepared_key = cache_keys.prepared_guide;
        let snapshot_cache_key = cache_keys.snapshot_completed_local;
        metrics.snapshot_completed_local_cache_hit = self
            .snapshot_completed_local_cache
            .as_ref()
            .is_some_and(|entry| entry.key == snapshot_cache_key);
        metrics.prepared_guide_key_ms = (clock_ms() - started).max(0.0);
        metrics.prepared_guide_cache_hit = self
            .prepared_guide_cache
            .as_ref()
            .is_some_and(|entry| entry.key == prepared_key);
        let (source, cache_root, _packages) = self.source()?;
        let paths = package_store::template_loader::TemplatePaths::new(&cache_root);
        let template_resolution =
            package_store::resolve_template_base_chain(&source, &paths, template_coordinate)
                .map_err(|error| format!("{operation}: resolve template chain: {error}"))?;
        if let Some(missing) = &template_resolution.missing {
            return Err(format!(
                "{operation}: template chain is incomplete; missing {missing}"
            ));
        }
        let started = clock_ms();
        let package_lock = with_template_chain(
            &package_lock,
            &template_resolution.chain,
            &self.package_materials,
            operation,
        )?;
        metrics.package_lock_ms += (clock_ms() - started).max(0.0);
        let diagnostics = site_build_diagnostics(&self.last_compile_diagnostics);
        let renderer =
            site_build::ProducerRef::new("publisher-template-rust", env!("CARGO_PKG_VERSION"));
        let preparation_key =
            publisher_runtime_preparation_key(&PublisherRuntimePreparationKeyPayload {
                schema: PUBLISHER_RUNTIME_PREPARATION_SCHEMA,
                recipe: PUBLISHER_RUNTIME_PREPARATION_RECIPE,
                engine_api: API_VERSION,
                project: &project_revision,
                package_lock: &package_lock,
                prepared_guide_key: &prepared_key,
                diagnostics: &diagnostics,
                template: &template,
                template_chain: &template_resolution.chain,
                renderer: &renderer,
                active_tables,
                run_uuid: run_uuid.as_deref(),
            })
            .map_err(|error| format!("{operation}: retained runtime key: {error}"))?;
        let retained = self.reuse_retained_publisher(&preparation_key);
        if let Some((handle, build)) = retained {
            metrics.site_build_cache_hit = true;
            metrics.total_ms = (clock_ms() - total_started).max(0.0);
            return Ok(PrepareSiteResult {
                handle: handle.clone(),
                build_id: handle,
                generator: "publisher".into(),
                site_build: build,
                metrics,
            });
        }

        // Publisher consumes the same renderer-neutral preparation as Cycle.
        // The render surface remains an implementation detail of this target.
        let started = clock_ms();
        let prepared = self.site_model_from_compile(
            &guide_options,
            operation,
            prepared_key.clone(),
            snapshot_cache_key,
        )?;
        metrics.prepared_guide_ms = (clock_ms() - started).max(0.0);
        if prepared.guide.package_id != project_id || prepared.guide.fhir_version != fhir_version {
            return Err(format!(
                "{operation}: PreparedGuide identity disagrees with compiled project"
            ));
        }
        let site_options = SiteOptions {
            active_tables,
            run_uuid: run_uuid.clone(),
            ..Default::default()
        };

        let started = clock_ms();
        let tree =
            package_store::template_loader::materialize(&source, &paths, template_coordinate)
                .map_err(|error| {
                    format!("{operation}: materialize {template_coordinate}: {error}")
                })?;
        metrics.template_materialize_ms = (clock_ms() - started).max(0.0);

        // Cross-version tooling may legitimately add support cores to the exact
        // closure (mCODE is R4 but also carries R5 core). Select the target core
        // from the compiled IG's FHIR version and require that exact coordinate
        // in the lock; counting every `*.core` package confuses execution input
        // with support context.
        let core = target_core_from_package_lock(&package_lock, &fhir_version, operation)?;
        let started = clock_ms();
        let runtime = site_producer::publisher_runtime::PublisherRuntime::assemble(
            &source,
            &cache_root,
            &core,
            &tree,
        )
        .map_err(|error| format!("{operation}: Publisher runtime: {error:#}"))?;

        let mut ready = BTreeMap::new();
        let mut objects = BTreeMap::new();
        for file in runtime.files() {
            let transformation = file
                .provenance
                .transformation
                .as_deref()
                .map(|value| format!("; transformation={value}"))
                .unwrap_or_default();
            insert_prepared_output(
                &mut ready,
                &mut objects,
                &file.path,
                file.bytes.clone(),
                &file.media_type,
                site_build::OutputProducer {
                    id: "publisher-runtime".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                Some(format!(
                    "{}; license={}; source={}{}",
                    file.provenance.source,
                    file.provenance.license,
                    file.provenance.source_path,
                    transformation
                )),
                PreparedOutputCollision::Reject,
            )?;
        }
        metrics.publisher_runtime_ms = (clock_ms() - started).max(0.0);

        let started = clock_ms();
        let config_json: Value = serde_json::from_slice(
            tree.get("config.json").ok_or_else(|| {
                format!(
                    "{operation}: no template config.json; Publisher template assembly is incomplete"
                )
            })?,
        )
        .map_err(|error| format!("{operation}: bad template config.json: {error}"))?;
        let layouts = tree
            .files()
            .iter()
            .filter_map(|(relative, bytes)| {
                relative
                    .strip_prefix("layouts/")
                    .and_then(|_| String::from_utf8(bytes.clone()).ok())
                    .map(|text| (format!("template/{relative}"), text))
            })
            .collect::<std::collections::HashMap<_, _>>();
        if layouts.is_empty() {
            return Err(format!(
                "{operation}: no template layouts; Publisher template assembly is incomplete"
            ));
        }
        let producer_inputs =
            site_producer::ProducerInputs::from_prepared(&prepared, &config_json, layouts, "en/")
                .map_err(|error| format!("{operation}: {error:#}"))?;
        let produced = site_producer::produce(&producer_inputs)
            .map_err(|error| format!("{operation}: {error:#}"))?;
        let resource_pages = produced.resource_pages;
        let mut site_files = std::collections::HashMap::new();
        for (relative, bytes) in tree.into_files() {
            let mounted = match relative.strip_prefix("includes/") {
                Some(name) => format!("_includes/{name}"),
                None => format!("template/{relative}"),
            };
            site_files.insert(PathBuf::from(format!("/site/{mounted}")), bytes);
        }

        for (name, body) in produced.pages {
            site_files.insert(PathBuf::from(format!("/site/{name}")), body.into_bytes());
        }
        for (name, body) in produced.data {
            site_files.insert(
                PathBuf::from(format!("/site/_data/{name}")),
                body.into_bytes(),
            );
        }
        for (name, body) in produced.includes {
            site_files.insert(
                PathBuf::from(format!("/site/_includes/{name}")),
                body.into_bytes(),
            );
        }
        stage_prepared_authored_files(
            &prepared,
            &mut site_files,
            &mut ready,
            &mut objects,
            &project_id,
        )?;
        metrics.publisher_model_ms = (clock_ms() - started).max(0.0);

        let started = clock_ms();
        let (render_source, _, _) = self.source_for_current_revision()?;
        let semantics = build_render_semantics(
            prepared_render_set(&prepared)?,
            Some(render_source),
            &site_files,
            &site_options,
        )?;
        let state = Rc::new(build_render_state_from_semantics(
            &semantics,
            &site_files,
            &site_options,
        )?);
        metrics.render_model_ms = (clock_ms() - started).max(0.0);

        let started = clock_ms();
        let pages = state.list_pages();
        add_page_relative_output_aliases(&mut ready, &pages)?;

        let tree_manifest = site_files
            .iter()
            .map(|(path, bytes)| {
                (
                    path.to_string_lossy().into_owned(),
                    site_build::ContentRef::of_bytes(bytes, None::<String>),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let tree_digest = site_build::sha256_canonical(&tree_manifest)
            .map_err(|error| format!("{operation}: hash mounted tree: {error}"))?;
        let mut parameters = BTreeMap::from([
            ("contract".into(), "publisher-site/v1".into()),
            ("mountedTreeSha256".into(), tree_digest.to_string()),
            ("preparedGuideSha256".into(), prepared_key.to_string()),
            (
                "publisherRuntimeRecipeSha256".into(),
                runtime.recipe_sha256().to_string(),
            ),
            (
                "buildEpochSecs".into(),
                guide_options.build_epoch_secs.to_string(),
            ),
            ("activeTables".into(), active_tables.to_string()),
        ]);
        if let Some(run_uuid) = &run_uuid {
            parameters.insert("runUuid".into(), run_uuid.clone());
        }
        let build = site_build::SiteBuild::new(
            project_revision,
            package_lock,
            site_build::RenderTarget {
                renderer,
                mode: site_build::RenderMode::NativeTemplate,
                fhir_version,
                template: Some(template.clone()),
                parameters: parameters.clone(),
            },
            site_build::RenderPlan::default(),
            site_build::ArtifactCatalog::from_records(Vec::new())
                .map_err(|error| format!("{operation}: artifact catalog: {error}"))?,
            diagnostics,
        )
        .map_err(|error| format!("{operation}: SiteBuild: {error}"))?
        .close()
        .map_err(|error| format!("{operation}: close SiteBuild: {error}"))?;

        let mut catalog = ready
            .iter()
            .map(|(path, output)| OutputDescriptor {
                path: path.clone(),
                kind: if is_static_asset(path.as_str()) {
                    "asset"
                } else {
                    "auxiliary"
                },
                media_type: output
                    .content
                    .media_type
                    .clone()
                    .expect("prepared output media type"),
                content: Some(output.content.clone()),
                title: None,
                subject: None,
                subject_page: None,
            })
            .collect::<Vec<_>>();
        for page in pages {
            let metadata = resource_pages.get(&page);
            let path = site_build::OutputPath::parse(page.clone())
                .map_err(|error| format!("{operation}: invalid page {page}: {error}"))?;
            catalog.push(OutputDescriptor {
                path,
                kind: "page",
                media_type: "text/html".into(),
                content: None,
                title: metadata.map(|metadata| metadata.title.clone()),
                subject: metadata.map(|metadata| OutputResourceSubject {
                    resource_type: metadata.resource_type.clone(),
                    id: metadata.id.clone(),
                }),
                subject_page: metadata.map(|metadata| match metadata.role {
                    site_producer::ResourcePageRole::Primary => OutputSubjectPage::Primary,
                    site_producer::ResourcePageRole::Companion => OutputSubjectPage::Companion,
                }),
            });
        }
        catalog.sort_by(|left, right| left.path.cmp(&right.path));
        for pair in catalog.windows(2) {
            if pair[0].path == pair[1].path {
                return Err(format!(
                    "{operation}: duplicate output path {}",
                    pair[0].path
                ));
            }
        }

        let renderer = site_build::RendererImplementation {
            id: "publisher-template-rust".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            recipe_sha256: site_build::sha256_canonical(&(template, &parameters))
                .map_err(|error| format!("{operation}: renderer recipe: {error}"))?,
        };
        let handle = build.site_build().build_id().to_string();
        self.retain_site_build(
            handle.clone(),
            SiteBuildRuntime::Publisher(PublisherBuildRuntime {
                preparation_key,
                state,
                runtime: Some(runtime),
                build: build.clone(),
                catalog,
                ready,
                objects,
                renderer,
                output_options: parameters,
            }),
        );
        metrics.catalog_ms = (clock_ms() - started).max(0.0);
        metrics.total_ms = (clock_ms() - total_started).max(0.0);
        Ok(PrepareSiteResult {
            handle: handle.clone(),
            build_id: handle,
            generator: "publisher".into(),
            site_build: build,
            metrics,
        })
    }

    fn site_outputs(&self, handle: &str) -> Result<OutputCatalogResult, String> {
        let runtime = self
            .site_builds
            .get(handle)
            .ok_or_else(|| format!("outputs: unknown build handle {handle}"))?;
        let SiteBuildRuntime::Publisher(runtime) = runtime else {
            return Err(
                "outputs: Cycle output catalog belongs to the external LiquidJS host".into(),
            );
        };
        let mut outputs = runtime.catalog.clone();
        for output in &mut outputs {
            if let Some(ready) = runtime.ready.get(&output.path) {
                output.content = Some(ready.content.clone());
            }
        }
        Ok(OutputCatalogResult {
            build_id: runtime.build.site_build().build_id().to_string(),
            outputs,
        })
    }

    fn render_site_output(&mut self, handle: &str, path: &str) -> Result<RenderSiteResult, String> {
        let path = site_build::OutputPath::parse(path.to_string())
            .map_err(|error| format!("render: invalid output path {path}: {error}"))?;
        let runtime = self
            .site_builds
            .get_mut(handle)
            .ok_or_else(|| format!("render: unknown build handle {handle}"))?;
        let SiteBuildRuntime::Publisher(runtime) = runtime else {
            return Err("render: Cycle outputs are rendered by the external LiquidJS host".into());
        };
        if let Some(output) = runtime.ready.get(&path) {
            return Ok(RenderSiteResult {
                path,
                media_type: output
                    .content
                    .media_type
                    .clone()
                    .ok_or_else(|| "render: prepared output has no media type".to_string())?,
                content: output.content.clone(),
                non_ready_fragments: 0,
            });
        }
        let declared = runtime.catalog.iter().any(|output| output.path == path);
        if !declared {
            return Err(format!("render: path {path} is not declared by outputs"));
        }
        let (html, reads) = runtime
            .state
            .render_page_tracked_by_name(path.as_str())
            .map_err(|error| format!("render {path}: {error}"))?;
        let html = runtime
            .runtime
            .as_ref()
            .map(|publisher| publisher.finish_html(&html))
            .unwrap_or(html);
        let non_ready_fragments = reads
            .observations()
            .values()
            .filter(|observation| matches!(observation, ArtifactObservation::NotReady { .. }))
            .count();
        let bytes = html.into_bytes();
        let content = site_build::ContentRef::of_bytes(&bytes, Some("text/html"));
        runtime
            .objects
            .entry(content.sha256.clone())
            .or_insert(bytes);
        runtime.ready.insert(
            path.clone(),
            PreparedOutput {
                content: content.clone(),
                producer: site_build::OutputProducer {
                    id: "publisher-page".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                source: Some(path.to_string()),
                owner: None,
            },
        );
        Ok(RenderSiteResult {
            path,
            media_type: "text/html".into(),
            content,
            non_ready_fragments,
        })
    }

    fn read_site_content(&self, handle: &str, digest: &str) -> Result<Vec<u8>, String> {
        let digest = site_build::Sha256Digest::parse(digest.to_string())
            .map_err(|error| format!("readContent: invalid digest: {error}"))?;
        let runtime = self
            .site_builds
            .get(handle)
            .ok_or_else(|| format!("readContent: unknown build handle {handle}"))?;
        let bytes = match runtime {
            SiteBuildRuntime::Publisher(runtime) => runtime.objects.get(&digest).cloned(),
            SiteBuildRuntime::Cycle(runtime) => runtime.objects.get(&digest).cloned(),
        }
        .ok_or_else(|| format!("readContent: object {digest} is absent from build {handle}"))?;
        if site_build::Sha256Digest::of_bytes(&bytes) != digest {
            return Err(format!(
                "readContent: object {digest} failed digest verification"
            ));
        }
        Ok(bytes)
    }

    fn finalize_site(&self, handle: &str) -> Result<site_build::SiteOutput, String> {
        let runtime = self
            .site_builds
            .get(handle)
            .ok_or_else(|| format!("finalize: unknown build handle {handle}"))?;
        let SiteBuildRuntime::Publisher(runtime) = runtime else {
            return Err(
                "finalize: Cycle finalization uses the internal external-renderer binding".into(),
            );
        };
        let missing = runtime
            .catalog
            .iter()
            .filter(|output| !runtime.ready.contains_key(&output.path))
            .map(|output| output.path.to_string())
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(format!(
                "finalize: {} declared outputs are not rendered: {}",
                missing.len(),
                missing.join(", ")
            ));
        }
        let files = runtime
            .ready
            .iter()
            .map(|(path, output)| site_build::SiteOutputFile {
                path: path.clone(),
                content: output.content.clone(),
                producer: output.producer.clone(),
                source: output.source.clone(),
                owner: output.owner.clone(),
            });
        let output = site_build::SiteOutput::new(
            &runtime.build,
            runtime.renderer.clone(),
            "publisher-site/v1",
            runtime.output_options.clone(),
            files,
        )
        .map_err(|error| format!("finalize: {error}"))?;
        output
            .verify_for(&runtime.build)
            .map_err(|error| format!("finalize: verify: {error}"))?;
        for file in output.files() {
            let bytes = runtime
                .objects
                .get(&file.content.sha256)
                .ok_or_else(|| format!("finalize: content object for {} is absent", file.path))?;
            file.content
                .verify(bytes)
                .map_err(|error| format!("finalize: verify {} bytes: {error}", file.path))?;
        }
        Ok(output)
    }

    fn finalize_external_site(
        &self,
        handle: &str,
        input_json: &str,
    ) -> Result<site_build::SiteOutput, String> {
        let runtime = self
            .site_builds
            .get(handle)
            .ok_or_else(|| format!("finalizeExternal: unknown build handle {handle}"))?;
        let SiteBuildRuntime::Cycle(runtime) = runtime else {
            return Err("finalizeExternal: handle does not name an external Cycle build".into());
        };
        let input: ExternalFinalizeInput = serde_json::from_str(input_json)
            .map_err(|error| format!("finalizeExternal: invalid input: {error}"))?;
        let catalog = input.catalog.into_iter().collect::<BTreeSet<_>>();
        if catalog.len() != input.files.len() {
            return Err(
                "finalizeExternal: catalog/file cardinality differs or catalog contains duplicates"
                    .into(),
            );
        }
        let file_paths = input
            .files
            .iter()
            .map(|file| file.path.clone())
            .collect::<BTreeSet<_>>();
        if file_paths.len() != input.files.len() {
            return Err("finalizeExternal: output files contain duplicate paths".into());
        }
        let missing = catalog.difference(&file_paths).cloned().collect::<Vec<_>>();
        let undeclared = file_paths.difference(&catalog).cloned().collect::<Vec<_>>();
        if !missing.is_empty() || !undeclared.is_empty() {
            return Err(format!(
                "finalizeExternal: incomplete catalog (missing: {}; undeclared: {})",
                missing
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", "),
                undeclared
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let output = site_build::SiteOutput::new(
            &runtime.build,
            input.renderer,
            input.output_schema,
            input.options,
            input.files,
        )
        .map_err(|error| format!("finalizeExternal: {error}"))?;
        output
            .verify_for(&runtime.build)
            .map_err(|error| format!("finalizeExternal: verify: {error}"))?;
        Ok(output)
    }
}

#[cfg(test)]
mod render_invalidation_tests {
    use super::*;

    const CONFIG: &str = "id: test\ncanonical: https://example.test\nfhirVersion: 4.0.1\n";

    fn resolved() -> ResolvedPackages {
        ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(CONFIG.as_bytes()),
            resolution_support: BTreeSet::from(["hl7.fhir.r4.core#4.0.1".into()]),
            labels: vec!["hl7.fhir.r4.core#4.0.1".into()],
        }
    }

    fn compilation(name: &str, parent: &str) -> SemanticCompilation {
        let fsh_source = format!("Profile: {name}\nParent: {parent}\n");
        let inputs = RenderSemanticInputs {
            config: CONFIG.into(),
            fsh: BTreeMap::from([("input/fsh/test.fsh".into(), fsh_source)]),
            predefined: BTreeMap::new(),
            page_listing: BTreeMap::from([("pagecontent".into(), vec!["index.md".into()])]),
        };
        let body = serde_json::json!({
            "resourceType": "StructureDefinition",
            "id": name
        });
        let result = CompileResult {
            resources: vec![CompiledResourceJs {
                filename: format!("StructureDefinition-{name}.json"),
                text: serde_json::to_string(&body).unwrap(),
                resource_type: Some("StructureDefinition".into()),
                id: Some(name.into()),
                url: Some(format!("https://example.test/StructureDefinition/{name}")),
                definition: None,
            }],
            diagnostics: vec![DiagnosticJs {
                severity: "information".into(),
                message: format!("compiled {name}"),
                file: None,
                line: None,
            }],
            timings: Timings::default(),
        };
        SemanticCompilation {
            key: semantic_compilation_key(&inputs, &resolved(), "test").unwrap(),
            compiled: vec![(
                PathBuf::from(format!("/__compiled__/StructureDefinition-{name}.json")),
                body,
            )],
            diagnostics: result.diagnostics.clone(),
            result,
        }
    }

    fn compile_project_reuse_engine() -> Engine {
        let active = compilation("Test", "Patient");
        let fsh = BTreeMap::from([(
            "input/fsh/test.fsh".into(),
            "Profile: Test\nParent: Patient\n".into(),
        )]);
        let site_files =
            BTreeMap::from([("input/pagecontent/index.md".into(), "b2xkIHByb3Nl".into())]);
        let mut engine = Engine {
            resolved_packages: Some(resolved()),
            page_listing: std::collections::HashMap::from([(
                "pagecontent".into(),
                vec!["index.md".into()],
            )]),
            ..Default::default()
        };
        engine.replace_active_compilation(active);
        engine.previous_compilation = None;
        engine.last_project = Some(CompiledProjectRevision {
            config: CONFIG.into(),
            fsh,
            predefined: BTreeMap::new(),
            site_files,
            resolved_packages: Some(resolved()),
        });
        engine
    }

    fn compile_project_args_for(parent: &str, site_body: &str) -> (String, String, String) {
        (
            serde_json::json!({
                "input/fsh/test.fsh": format!("Profile: Test\nParent: {parent}\n")
            })
            .to_string(),
            "{}".into(),
            serde_json::json!({
                "input/pagecontent/index.md": site_body
            })
            .to_string(),
        )
    }

    fn compile_project_args() -> (String, String, String) {
        compile_project_args_for("Patient", "bmV3IHByb3Nl")
    }

    #[test]
    fn compile_project_reuses_exact_semantic_result_but_replaces_site_revision() {
        let mut engine = compile_project_reuse_engine();
        let (files, predefined, site_files) = compile_project_args();

        let result = engine
            .compile_project(&files, CONFIG, &predefined, &site_files)
            .expect("prose-only revision reuses the exact compile");

        assert_eq!(engine.derived_cache_hits.compile_project, 1);
        assert_eq!(result.resources.len(), 1);
        assert_eq!(
            result.resources[0].filename,
            "StructureDefinition-Test.json"
        );
        assert_eq!(
            engine
                .last_project
                .as_ref()
                .unwrap()
                .site_files
                .get("input/pagecontent/index.md")
                .map(String::as_str),
            Some("bmV3IHByb3Nl")
        );
    }

    #[test]
    fn compile_project_reuse_invalidates_for_every_compiler_visible_identity_change() {
        let (baseline_files, baseline_predefined, baseline_site_files) = compile_project_args();

        let cases = [
            (
                "config",
                baseline_files.clone(),
                format!("{CONFIG}status: active\n"),
                baseline_predefined.clone(),
                baseline_site_files.clone(),
            ),
            (
                "FSH",
                serde_json::json!({
                    "input/fsh/test.fsh": "Profile: Test\nParent: Observation\n"
                })
                .to_string(),
                CONFIG.into(),
                baseline_predefined.clone(),
                baseline_site_files.clone(),
            ),
            (
                "predefined",
                baseline_files.clone(),
                CONFIG.into(),
                serde_json::json!({
                    "input/resources/Patient-p.json": {
                        "resourceType": "Patient",
                        "id": "p"
                    }
                })
                .to_string(),
                serde_json::json!({
                    "input/pagecontent/index.md": "bmV3IHByb3Nl",
                    "input/resources/Patient-p.json":
                        "eyJyZXNvdXJjZVR5cGUiOiJQYXRpZW50IiwiaWQiOiJwIn0="
                })
                .to_string(),
            ),
            (
                "page listing",
                baseline_files.clone(),
                CONFIG.into(),
                baseline_predefined.clone(),
                serde_json::json!({
                    "input/pagecontent/index.md": "bmV3IHByb3Nl",
                    "input/pagecontent/next.md": "bmV4dA=="
                })
                .to_string(),
            ),
        ];

        for (label, files, config, predefined, site_files) in cases {
            let mut engine = compile_project_reuse_engine();
            if label == "config" {
                engine.resolved_packages.as_mut().unwrap().config_sha256 =
                    site_build::Sha256Digest::of_bytes(config.as_bytes());
            }
            let error = engine
                .compile_project(&files, &config, &predefined, &site_files)
                .err()
                .expect("changed identity must execute the compiler path");
            assert!(
                error.contains("engine not initialized"),
                "{label} change unexpectedly reused the prior compile: {error}"
            );
            assert_eq!(engine.derived_cache_hits.compile_project, 0, "{label}");
        }

        let mut engine = compile_project_reuse_engine();
        engine
            .resolved_packages
            .as_mut()
            .unwrap()
            .labels
            .push("hl7.terminology.r4#7.2.0".into());
        let error = engine
            .compile_project(
                &baseline_files,
                CONFIG,
                &baseline_predefined,
                &baseline_site_files,
            )
            .err()
            .expect("changed closure must execute the compiler path");
        assert!(error.contains("engine not initialized"), "{error}");
        assert_eq!(engine.derived_cache_hits.compile_project, 0);
    }

    #[test]
    fn compile_project_reuses_previous_exact_compilation_across_a_b_a() {
        let mut engine = compile_project_reuse_engine();
        engine.replace_active_compilation(compilation("Other", "Observation"));
        assert_eq!(
            engine
                .previous_compilation
                .as_ref()
                .unwrap()
                .result
                .resources[0]
                .filename,
            "StructureDefinition-Test.json"
        );

        let (files, predefined, site_files) = compile_project_args_for("Patient", "Y3VycmVudCBB");
        let result = engine
            .compile_project(&files, CONFIG, &predefined, &site_files)
            .expect("A -> B -> A must restore the previous exact compilation");

        assert_eq!(engine.derived_cache_hits.compile_project, 1);
        assert_eq!(
            result.resources[0].filename,
            "StructureDefinition-Test.json"
        );
        assert_eq!(
            engine.last_compiled[0].1.get("id").and_then(Value::as_str),
            Some("Test")
        );
        assert_eq!(engine.last_compile_diagnostics[0].message, "compiled Test");
        assert_eq!(
            engine
                .previous_compilation
                .as_ref()
                .unwrap()
                .result
                .resources[0]
                .filename,
            "StructureDefinition-Other.json"
        );
        assert_eq!(
            engine
                .last_project
                .as_ref()
                .unwrap()
                .site_files
                .get("input/pagecontent/index.md")
                .map(String::as_str),
            Some("Y3VycmVudCBB")
        );
    }

    #[test]
    fn failed_compile_preserves_current_previous_and_authored_revision() {
        let mut engine = compile_project_reuse_engine();
        engine.replace_active_compilation(compilation("Other", "Observation"));
        engine
            .last_project
            .as_mut()
            .unwrap()
            .site_files
            .insert("input/pagecontent/index.md".into(), "cHJpb3I=".into());
        let current_key = engine.last_compile_key.clone();
        let previous_key = engine
            .previous_compilation
            .as_ref()
            .map(|entry| entry.key.clone());

        let (files, predefined, site_files) =
            compile_project_args_for("Encounter", "ZmFpbGVkIGF1dGhvcmVk");
        let error = engine
            .compile_project(&files, CONFIG, &predefined, &site_files)
            .err()
            .expect("a third uncached compilation needs the unavailable compiler source");
        assert!(error.contains("engine not initialized"), "{error}");
        assert_eq!(engine.last_compile_key, current_key);
        assert_eq!(
            engine
                .previous_compilation
                .as_ref()
                .map(|entry| entry.key.clone()),
            previous_key
        );
        assert_eq!(
            engine
                .last_project
                .as_ref()
                .unwrap()
                .site_files
                .get("input/pagecontent/index.md")
                .map(String::as_str),
            Some("cHJpb3I=")
        );
        assert_eq!(engine.derived_cache_hits.compile_project, 0);
    }

    #[test]
    fn third_distinct_success_evicts_only_the_oldest_compilation() {
        let mut engine = compile_project_reuse_engine();
        engine.replace_active_compilation(compilation("Other", "Observation"));
        engine.replace_active_compilation(compilation("Third", "Encounter"));
        assert_eq!(
            engine
                .previous_compilation
                .as_ref()
                .unwrap()
                .result
                .resources[0]
                .filename,
            "StructureDefinition-Other.json"
        );

        let (a_files, predefined, site_files) = compile_project_args();
        let error = engine
            .compile_project(&a_files, CONFIG, &predefined, &site_files)
            .err()
            .expect("the evicted oldest compilation must run the compiler");
        assert!(error.contains("engine not initialized"), "{error}");
        assert_eq!(engine.derived_cache_hits.compile_project, 0);

        let b_files = serde_json::json!({
            "input/fsh/test.fsh": "Profile: Other\nParent: Observation\n"
        })
        .to_string();
        let result = engine
            .compile_project(&b_files, CONFIG, &predefined, &site_files)
            .expect("the immediately previous compilation remains reusable");
        assert_eq!(
            result.resources[0].filename,
            "StructureDefinition-Other.json"
        );
        assert_eq!(engine.derived_cache_hits.compile_project, 1);
    }
}

#[cfg(test)]
mod site_facade_tests {
    use super::*;
    use std::collections::HashMap;

    fn authored(role: site_build::AuthoredFileRole, path: &str) -> site_build::AuthoredFile {
        site_build::AuthoredFile {
            role,
            path: site_build::PreparedPath::parse(path).unwrap(),
            mime: "text/plain".into(),
            content: path.as_bytes().to_vec(),
            source_reads: BTreeSet::from([site_build::PreparedPath::parse(format!(
                "input/{path}"
            ))
            .unwrap()]),
        }
    }

    #[test]
    fn authored_roles_cannot_project_to_one_publisher_path() {
        let error = validate_authored_projections(&[
            authored(site_build::AuthoredFileRole::ResourceContent, "shared.md"),
            authored(site_build::AuthoredFileRole::Include, "shared.md"),
        ])
        .unwrap_err();
        assert!(error.contains("authored projection collision"));
        assert!(error.contains("tree:/site/_includes/shared.md"));
        assert!(error.contains("ResourceContent"));
        assert!(error.contains("Include"));

        let error = validate_authored_projections(&[
            authored(site_build::AuthoredFileRole::PageContent, "guide.md"),
            authored(site_build::AuthoredFileRole::Image, "en/guide.html"),
        ])
        .unwrap_err();
        assert!(error.contains("output:en/guide.html"));
    }

    #[test]
    fn authored_output_override_is_the_only_explicit_output_precedence() {
        let mut outputs = BTreeMap::new();
        let mut objects = BTreeMap::new();
        let runtime = site_build::OutputProducer {
            id: "runtime".into(),
            version: "1".into(),
        };
        insert_prepared_output(
            &mut outputs,
            &mut objects,
            "logo.svg",
            b"runtime".to_vec(),
            "image/svg+xml",
            runtime.clone(),
            None,
            PreparedOutputCollision::Reject,
        )
        .unwrap();
        assert!(insert_prepared_output(
            &mut outputs,
            &mut objects,
            "logo.svg",
            b"other runtime".to_vec(),
            "image/svg+xml",
            runtime,
            None,
            PreparedOutputCollision::Reject,
        )
        .unwrap_err()
        .contains("prepared output collision"));
        insert_prepared_output(
            &mut outputs,
            &mut objects,
            "logo.svg",
            b"authored".to_vec(),
            "image/svg+xml",
            site_build::OutputProducer {
                id: "publisher-authored-asset".into(),
                version: "guide".into(),
            },
            None,
            PreparedOutputCollision::AuthoredOverridesRenderer,
        )
        .unwrap();
        assert_eq!(
            outputs[&site_build::OutputPath::parse("logo.svg").unwrap()]
                .producer
                .id,
            "publisher-authored-asset"
        );
    }

    fn closed(project: &str) -> site_build::ClosedSiteBuild {
        site_build::SiteBuild::new(
            site_build::ProjectRevision {
                project_id: project.into(),
                revision: format!("revision-{project}"),
                sources: site_build::SourceManifest::default(),
            },
            site_build::PackageLock::default(),
            site_build::RenderTarget {
                renderer: site_build::ProducerRef::new(
                    "publisher-template-rust",
                    env!("CARGO_PKG_VERSION"),
                ),
                mode: site_build::RenderMode::NativeTemplate,
                fhir_version: "4.0.1".into(),
                template: None,
                parameters: BTreeMap::from([("contract".into(), "publisher-site/v1".into())]),
            },
            site_build::RenderPlan::default(),
            site_build::ArtifactCatalog::default(),
            BTreeSet::new(),
        )
        .unwrap()
        .close()
        .unwrap()
    }

    fn publisher_runtime(project: &str, marker: &str) -> PublisherBuildRuntime {
        let compiled = vec![(
            PathBuf::from("/__ig__/ImplementationGuide-demo.json"),
            serde_json::json!({
                "resourceType":"ImplementationGuide",
                "id":"demo",
                "packageId":project,
                "fhirVersion":["4.0.1"]
            }),
        )];
        let site_files = HashMap::from([
            (
                PathBuf::from("/site/en/a.html"),
                format!("---\n---\n<p>{marker}-a</p>").into_bytes(),
            ),
            (
                PathBuf::from("/site/en/b.html"),
                format!("---\n---\n<p>{marker}-b</p>").into_bytes(),
            ),
        ]);
        let state = Rc::new(
            build_render_state(&compiled, None, &site_files, &SiteOptions::default()).unwrap(),
        );
        let asset_path = site_build::OutputPath::parse("assets/app.css").unwrap();
        let asset_bytes = format!("/* {marker} */").into_bytes();
        let asset_content = site_build::ContentRef::of_bytes(&asset_bytes, Some("text/css"));
        let asset = PreparedOutput {
            content: asset_content.clone(),
            producer: site_build::OutputProducer {
                id: "fixture-asset".into(),
                version: "1".into(),
            },
            source: Some("fixture".into()),
            owner: None,
        };
        let catalog = vec![
            OutputDescriptor {
                path: site_build::OutputPath::parse("en/a.html").unwrap(),
                kind: "page",
                media_type: "text/html".into(),
                content: None,
                title: None,
                subject: None,
                subject_page: None,
            },
            OutputDescriptor {
                path: site_build::OutputPath::parse("en/b.html").unwrap(),
                kind: "page",
                media_type: "text/html".into(),
                content: None,
                title: None,
                subject: None,
                subject_page: None,
            },
            OutputDescriptor {
                path: asset_path.clone(),
                kind: "asset",
                media_type: "text/css".into(),
                content: Some(asset.content.clone()),
                title: None,
                subject: None,
                subject_page: None,
            },
        ];
        PublisherBuildRuntime {
            preparation_key: site_build::Sha256Digest::of_bytes(
                format!("{project}\0{marker}").as_bytes(),
            ),
            state,
            runtime: None,
            build: closed(project),
            catalog,
            ready: BTreeMap::from([(asset_path, asset)]),
            objects: BTreeMap::from([(asset_content.sha256, asset_bytes)]),
            renderer: site_build::RendererImplementation {
                id: "publisher-template-rust".into(),
                version: "1".into(),
                recipe_sha256: site_build::Sha256Digest::of_bytes(marker.as_bytes()),
            },
            output_options: BTreeMap::from([("marker".into(), marker.into())]),
        }
    }

    fn engine_with(project: &str, marker: &str) -> (Engine, String) {
        let runtime = publisher_runtime(project, marker);
        let handle = runtime.build.site_build().build_id().to_string();
        let mut engine = Engine::default();
        engine.retain_site_build(handle.clone(), SiteBuildRuntime::Publisher(runtime));
        (engine, handle)
    }

    fn retain_publisher(engine: &mut Engine, project: &str, marker: &str) -> String {
        let runtime = publisher_runtime(project, marker);
        let handle = runtime.build.site_build().build_id().to_string();
        engine.retain_site_build(handle.clone(), SiteBuildRuntime::Publisher(runtime));
        handle
    }

    #[test]
    fn outputs_is_complete_before_render_and_finalize_is_verified() {
        let (mut engine, handle) = engine_with("catalog.test", "one");
        let catalog = engine.site_outputs(&handle).unwrap();
        assert_eq!(catalog.outputs.len(), 3);
        assert_eq!(
            catalog
                .outputs
                .iter()
                .filter(|entry| entry.kind == "page")
                .count(),
            2
        );
        assert!(catalog
            .outputs
            .iter()
            .find(|entry| entry.kind == "asset")
            .unwrap()
            .content
            .is_some());
        assert!(engine
            .finalize_site(&handle)
            .unwrap_err()
            .contains("2 declared outputs"));

        for path in ["en/a.html", "en/b.html"] {
            let rendered = engine.render_site_output(&handle, path).unwrap();
            assert_eq!(rendered.media_type, "text/html");
            let bytes = engine
                .read_site_content(&handle, rendered.content.sha256.as_str())
                .unwrap();
            rendered.content.verify(&bytes).unwrap();
        }
        let asset = engine
            .render_site_output(&handle, "assets/app.css")
            .unwrap();
        assert_eq!(asset.media_type, "text/css");
        assert_eq!(
            serde_json::to_value(asset).unwrap()["mediaType"],
            "text/css"
        );
        let output = engine.finalize_site(&handle).unwrap();
        output.verify().unwrap();
        assert_eq!(output.files().len(), 3);
        assert_eq!(output.input_build_id().as_str(), handle);
    }

    #[test]
    fn pages_render_in_any_order_without_advancing_the_handle() {
        let (mut left, handle) = engine_with("order.test", "same");
        let b_then_a = ["en/b.html", "en/a.html"]
            .map(|path| left.render_site_output(&handle, path).unwrap().content);
        assert!(left.site_builds.contains_key(&handle));

        let (mut right, other_handle) = engine_with("order.test", "same");
        assert_eq!(handle, other_handle);
        let a_then_b = ["en/a.html", "en/b.html"].map(|path| {
            right
                .render_site_output(&other_handle, path)
                .unwrap()
                .content
        });
        assert_eq!(b_then_a[1], a_then_b[0]);
        assert_eq!(b_then_a[0], a_then_b[1]);
        assert_eq!(
            left.finalize_site(&handle).unwrap().output_id(),
            right.finalize_site(&other_handle).unwrap().output_id()
        );
    }

    #[test]
    fn immutable_handles_are_isolated_from_other_builds() {
        let (mut engine, first) = engine_with("first.test", "first");
        let second = retain_publisher(&mut engine, "second.test", "second");
        let first_output = engine.render_site_output(&first, "en/a.html").unwrap();
        let second_output = engine.render_site_output(&second, "en/a.html").unwrap();
        assert_ne!(first, second);
        assert_ne!(first_output.content, second_output.content);
        let first_bytes = engine
            .read_site_content(&first, first_output.content.sha256.as_str())
            .unwrap();
        assert_eq!(String::from_utf8(first_bytes).unwrap(), "<p>first-a</p>");
    }

    #[test]
    fn successful_prepares_retain_only_current_and_immediately_previous_builds() {
        let mut engine = Engine::default();
        let first = retain_publisher(&mut engine, "retain.first", "first");
        let second = retain_publisher(&mut engine, "retain.second", "second");

        assert_eq!(engine.site_builds.len(), RETAINED_SITE_BUILD_LIMIT);
        assert!(engine.site_outputs(&first).is_ok());
        assert!(engine.site_outputs(&second).is_ok());

        let third = retain_publisher(&mut engine, "retain.third", "third");
        assert_eq!(engine.site_builds.len(), RETAINED_SITE_BUILD_LIMIT);
        assert_eq!(
            engine
                .site_build_generations
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![second.clone(), third.clone()]
        );
        assert!(engine
            .site_outputs(&first)
            .err()
            .expect("oldest handle must be evicted")
            .contains("unknown build handle"));
        assert!(engine.site_outputs(&second).is_ok());
        assert!(engine.site_outputs(&third).is_ok());
    }

    #[test]
    fn preparing_an_existing_handle_refreshes_recency_without_growing_the_bound() {
        let mut engine = Engine::default();
        let first = retain_publisher(&mut engine, "refresh.first", "first");
        let second = retain_publisher(&mut engine, "refresh.second", "second");
        assert_eq!(
            retain_publisher(&mut engine, "refresh.first", "first"),
            first
        );
        assert_eq!(engine.site_builds.len(), RETAINED_SITE_BUILD_LIMIT);
        assert_eq!(
            engine
                .site_build_generations
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![second.clone(), first.clone()]
        );

        let third = retain_publisher(&mut engine, "refresh.third", "third");
        assert!(engine.site_outputs(&second).is_err());
        assert!(engine.site_outputs(&first).is_ok());
        assert!(engine.site_outputs(&third).is_ok());
    }

    #[test]
    fn retained_publisher_probe_preserves_memoized_runtime_and_obeys_eviction() {
        let mut engine = Engine::default();
        let first_runtime = publisher_runtime("reuse.first", "first");
        let first_key = first_runtime.preparation_key.clone();
        let first_state = first_runtime.state.clone();
        let first = first_runtime.build.site_build().build_id().to_string();
        engine.retain_site_build(first.clone(), SiteBuildRuntime::Publisher(first_runtime));
        engine.render_site_output(&first, "en/a.html").unwrap();

        let cycle_build = closed("reuse.cycle");
        let cycle = cycle_build.site_build().build_id().to_string();
        engine.retain_site_build(
            cycle.clone(),
            SiteBuildRuntime::Cycle(CycleBuildRuntime {
                build: cycle_build,
                objects: BTreeMap::new(),
            }),
        );
        let (reused, _) = engine
            .reuse_retained_publisher(&first_key)
            .expect("A -> Cycle -> A must find the still-retained Publisher runtime");
        assert_eq!(reused, first);
        assert_eq!(
            engine
                .site_build_generations
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![cycle, first.clone()]
        );
        let SiteBuildRuntime::Publisher(runtime) = engine.site_builds.get(&first).unwrap() else {
            panic!("reused handle changed generator");
        };
        assert!(Rc::ptr_eq(&runtime.state, &first_state));
        assert!(runtime
            .ready
            .contains_key(&site_build::OutputPath::parse("en/a.html").unwrap()));
        assert_eq!(engine.derived_cache_hits.retained_publisher_runtime, 1);

        let order_before_miss = engine.site_build_generations.clone();
        assert!(engine
            .reuse_retained_publisher(&site_build::Sha256Digest::of_bytes(b"different"))
            .is_none());
        assert_eq!(engine.site_build_generations, order_before_miss);

        let mut evicted = Engine::default();
        let old_runtime = publisher_runtime("evict.first", "first");
        let old_key = old_runtime.preparation_key.clone();
        let old = old_runtime.build.site_build().build_id().to_string();
        evicted.retain_site_build(old, SiteBuildRuntime::Publisher(old_runtime));
        retain_publisher(&mut evicted, "evict.second", "second");
        retain_publisher(&mut evicted, "evict.third", "third");
        assert!(evicted.reuse_retained_publisher(&old_key).is_none());
        assert_eq!(evicted.derived_cache_hits.retained_publisher_runtime, 0);
    }

    #[test]
    fn failed_prepare_preserves_every_retained_generation() {
        let mut engine = Engine::default();
        let first = retain_publisher(&mut engine, "failure.first", "first");
        let second = retain_publisher(&mut engine, "failure.second", "second");
        let before = engine.site_build_generations.clone();

        let error = engine
            .prepare_site(r#"{"generator":"publisher","templateCoordinate":"","buildEpochSecs":1}"#)
            .err()
            .expect("empty template coordinate must fail");
        assert!(error.contains("template coordinate is empty"));
        assert_eq!(engine.site_build_generations, before);
        assert_eq!(engine.site_builds.len(), RETAINED_SITE_BUILD_LIMIT);
        assert!(engine.site_outputs(&first).is_ok());
        assert!(engine.site_outputs(&second).is_ok());
    }

    #[test]
    fn publisher_lock_includes_authenticated_template_base_chain() {
        let core = site_build::PackageCoordinate::parse("hl7.fhir.r4.core#4.0.1").unwrap();
        let compile_lock = site_build::PackageLock::from_packages([site_build::LockedPackage {
            coordinate: core,
            content: site_build::ContentRef::of_bytes(b"core", None::<String>),
            dependencies: BTreeSet::new(),
        }])
        .unwrap();
        let parent = "base.template#1.0.0";
        let child = "child.template#2.0.0";
        let materials = BTreeMap::from([
            (
                parent.into(),
                MountedPackage {
                    content: site_build::ContentRef::of_bytes(b"parent", None::<String>),
                    declared_dependencies: BTreeMap::new(),
                },
            ),
            (
                child.into(),
                MountedPackage {
                    content: site_build::ContentRef::of_bytes(b"child", None::<String>),
                    declared_dependencies: BTreeMap::from([(
                        "base.template".into(),
                        "1.0.0".into(),
                    )]),
                },
            ),
        ]);
        let lock = with_template_chain(
            &compile_lock,
            &[parent.into(), child.into()],
            &materials,
            "test",
        )
        .unwrap();
        let parent = site_build::PackageCoordinate::parse(parent).unwrap();
        let child = site_build::PackageCoordinate::parse(child).unwrap();
        assert_eq!(lock.get(&parent).unwrap().content.byte_length, 6);
        assert_eq!(lock.get(&child).unwrap().content.byte_length, 5);
        assert_eq!(
            lock.get(&child).unwrap().dependencies,
            BTreeSet::from([parent])
        );
    }

    #[test]
    fn publisher_runtime_preparation_key_has_the_exact_invalidation_boundary() {
        let project = site_build::ProjectRevision {
            project_id: "key.test".into(),
            revision: "revision-a".into(),
            sources: site_build::SourceManifest::default(),
        };
        let core = site_build::PackageCoordinate::parse("hl7.fhir.r4.core#4.0.1").unwrap();
        let template = site_build::PackageCoordinate::parse("demo.template#1.0.0").unwrap();
        let lock = |template_bytes: &[u8]| {
            site_build::PackageLock::from_packages([
                site_build::LockedPackage {
                    coordinate: core.clone(),
                    content: site_build::ContentRef::of_bytes(b"core", None::<String>),
                    dependencies: BTreeSet::new(),
                },
                site_build::LockedPackage {
                    coordinate: template.clone(),
                    content: site_build::ContentRef::of_bytes(template_bytes, None::<String>),
                    dependencies: BTreeSet::new(),
                },
            ])
            .unwrap()
        };
        let lock_a = lock(b"template-a");
        let prepared = site_build::Sha256Digest::of_bytes(b"prepared-a");
        let diagnostics = BTreeSet::new();
        let chain = vec![template.to_string()];
        let renderer = site_build::ProducerRef::new("publisher-template-rust", "1");
        let key = |project: &site_build::ProjectRevision,
                   package_lock: &site_build::PackageLock,
                   prepared_guide_key: &site_build::Sha256Digest,
                   diagnostics: &BTreeSet<site_build::BuildDiagnostic>,
                   template: &site_build::PackageCoordinate,
                   template_chain: &[String],
                   renderer: &site_build::ProducerRef,
                   active_tables: bool,
                   run_uuid: Option<&str>,
                   recipe: &str| {
            publisher_runtime_preparation_key(&PublisherRuntimePreparationKeyPayload {
                schema: PUBLISHER_RUNTIME_PREPARATION_SCHEMA,
                recipe,
                engine_api: API_VERSION,
                project,
                package_lock,
                prepared_guide_key,
                diagnostics,
                template,
                template_chain,
                renderer,
                active_tables,
                run_uuid,
            })
            .unwrap()
        };
        let baseline = key(
            &project,
            &lock_a,
            &prepared,
            &diagnostics,
            &template,
            &chain,
            &renderer,
            true,
            Some("run-a"),
            PUBLISHER_RUNTIME_PREPARATION_RECIPE,
        );

        let mut changed_project = project.clone();
        changed_project.revision = "revision-b".into();
        let mut changed_diagnostics = diagnostics.clone();
        changed_diagnostics.insert(site_build::BuildDiagnostic::new(
            site_build::DiagnosticSeverity::Warning,
            "changed",
            "changed",
        ));
        let changed_template = site_build::PackageCoordinate::parse("demo.template#2.0.0").unwrap();
        let changed_renderer = site_build::ProducerRef::new("publisher-template-rust", "2");
        let changed_prepared = site_build::Sha256Digest::of_bytes(b"prepared-b");
        let cases = [
            key(
                &changed_project,
                &lock_a,
                &prepared,
                &diagnostics,
                &template,
                &chain,
                &renderer,
                true,
                Some("run-a"),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE,
            ),
            key(
                &project,
                &lock(b"template-b"),
                &prepared,
                &diagnostics,
                &template,
                &chain,
                &renderer,
                true,
                Some("run-a"),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE,
            ),
            key(
                &project,
                &lock_a,
                &changed_prepared,
                &diagnostics,
                &template,
                &chain,
                &renderer,
                true,
                Some("run-a"),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE,
            ),
            key(
                &project,
                &lock_a,
                &prepared,
                &changed_diagnostics,
                &template,
                &chain,
                &renderer,
                true,
                Some("run-a"),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE,
            ),
            key(
                &project,
                &lock_a,
                &prepared,
                &diagnostics,
                &changed_template,
                &chain,
                &renderer,
                true,
                Some("run-a"),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE,
            ),
            key(
                &project,
                &lock_a,
                &prepared,
                &diagnostics,
                &template,
                &["base.template#1.0.0".into(), template.to_string()],
                &renderer,
                true,
                Some("run-a"),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE,
            ),
            key(
                &project,
                &lock_a,
                &prepared,
                &diagnostics,
                &template,
                &chain,
                &changed_renderer,
                true,
                Some("run-a"),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE,
            ),
            key(
                &project,
                &lock_a,
                &prepared,
                &diagnostics,
                &template,
                &chain,
                &renderer,
                false,
                Some("run-a"),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE,
            ),
            key(
                &project,
                &lock_a,
                &prepared,
                &diagnostics,
                &template,
                &chain,
                &renderer,
                true,
                Some("run-b"),
                PUBLISHER_RUNTIME_PREPARATION_RECIPE,
            ),
            key(
                &project,
                &lock_a,
                &prepared,
                &diagnostics,
                &template,
                &chain,
                &renderer,
                true,
                Some("run-a"),
                "publisher-template-rust.changed-recipe/v2",
            ),
        ];
        assert!(cases.into_iter().all(|changed| changed != baseline));
    }

    #[test]
    fn external_finalizer_requires_the_complete_declared_catalog() {
        let build = closed("cycle.test");
        let handle = build.site_build().build_id().to_string();
        let engine = Engine {
            site_builds: BTreeMap::from([(
                handle.clone(),
                SiteBuildRuntime::Cycle(CycleBuildRuntime {
                    build,
                    objects: BTreeMap::new(),
                }),
            )]),
            ..Default::default()
        };
        let file = site_build::SiteOutputFile {
            path: site_build::OutputPath::parse("index.html").unwrap(),
            content: site_build::ContentRef::of_bytes(b"page", Some("text/html")),
            producer: site_build::OutputProducer {
                id: "cycle-page".into(),
                version: "2".into(),
            },
            source: Some("cycle".into()),
            owner: None,
        };
        let renderer = site_build::RendererImplementation {
            id: "cycle-site".into(),
            version: "2".into(),
            recipe_sha256: site_build::Sha256Digest::of_bytes(b"cycle recipe"),
        };
        let incomplete = serde_json::json!({
            "renderer": renderer,
            "outputSchema": "cycle-static/v1",
            "catalog": ["index.html", "assets/app.css"],
            "files": [file]
        });
        assert!(engine
            .finalize_external_site(&handle, &incomplete.to_string())
            .unwrap_err()
            .contains("cardinality"));

        let complete = serde_json::json!({
            "renderer": renderer,
            "outputSchema": "cycle-static/v1",
            "catalog": ["index.html"],
            "files": [file]
        });
        let output = engine
            .finalize_external_site(&handle, &complete.to_string())
            .unwrap();
        output.verify().unwrap();
        assert_eq!(output.files().len(), 1);
    }

    #[test]
    fn publisher_prepare_owns_template_and_authored_assembly() {
        let core_manifest = serde_json::json!({
            "name":"hl7.fhir.r4.core",
            "version":"4.0.1",
            "fhirVersions":["4.0.1"]
        })
        .to_string();
        let support_core_manifest = serde_json::json!({
            "name":"hl7.fhir.r5.core",
            "version":"5.0.0",
            "fhirVersions":["5.0.0"]
        })
        .to_string();
        let template_manifest = serde_json::json!({
            "name":"demo.template",
            "version":"1.0.0",
            "type":"fhir.template"
        })
        .to_string();
        let config_json = r#"{"defaults":{"Any":{"template-base":"template/layouts/default.html","base":"{{[type]}}-{{[id]}}.html"},"ValueSet":{"template-base":"template/layouts/default.html","base":"published-{{[id]}}-landing.html"}}}"#;
        let bundles = serde_json::json!([
            {
                "label":"hl7.fhir.r4.core#4.0.1",
                "files":{
                    "package.json":base64_encode(core_manifest.as_bytes()),
                    "other/fhir.css":base64_encode(b"/* fhir */"),
                    "other/icon_element.gif":base64_encode(b"icon"),
                    "other/tbl_spacer.png":base64_encode(b"spacer")
                }
            },
            {
                "label":"hl7.fhir.r5.core#5.0.0",
                "files":{
                    "package.json":base64_encode(support_core_manifest.as_bytes())
                }
            },
            {
                "label":"demo.template#1.0.0",
                "files":{
                    "package.json":base64_encode(template_manifest.as_bytes()),
                    "config.json":base64_encode(config_json.as_bytes()),
                    "layouts/default.html":base64_encode(b"---\n---\n{{content}}"),
                    "includes/template-page-md.html":base64_encode(
                        br#"<link href="assets/app.css"><img src="icon_element.gif">{{content}}"#
                    ),
                    "content/assets/app.css":base64_encode(b"body{}")
                }
            }
        ]);
        let config = "id: facade.test\ncanonical: https://example.org/facade\nname: FacadeTest\nstatus: draft\nversion: 1.0.0\nfhirVersion: 4.0.1\n";
        let resolved = ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support: BTreeSet::new(),
            labels: vec![
                "hl7.fhir.r4.core#4.0.1".into(),
                "hl7.fhir.r5.core#5.0.0".into(),
            ],
        };
        let mut engine = Engine::default();
        engine.init(&bundles.to_string()).unwrap();
        engine.last_project = Some(CompiledProjectRevision {
            config: config.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::from([
                (
                    "input/pagecontent/index.md".into(),
                    base64_encode(b"# Intro"),
                ),
                (
                    "input/pagecontent/conformance-patients.md".into(),
                    base64_encode(
                        br#"# Conformance: Patients

<div>{% include patients-with-cancer-condition.svg %}</div>
"#,
                    ),
                ),
                ("input/images/logo.svg".into(), base64_encode(b"<svg/>")),
            ]),
            resolved_packages: Some(resolved),
        });
        engine.last_compiled = vec![
            (
                PathBuf::from("/__compiled__/ValueSet-renamed-status.json"),
                serde_json::json!({
                    "resourceType":"ValueSet",
                    "id":"renamed-status",
                    "url":"https://example.org/facade/ValueSet/renamed-status",
                    "name":"RenamedStatus",
                    "status":"draft"
                }),
            ),
            (
                PathBuf::from("/__ig__/ImplementationGuide-facade-test.json"),
                serde_json::json!({
                "resourceType":"ImplementationGuide",
                "id":"facade-test",
                "packageId":"facade.test",
                "url":"https://example.org/facade/ImplementationGuide/facade-test",
                "name":"FacadeTest",
                "status":"draft",
                "version":"1.0.0",
                "fhirVersion":["4.0.1"],
                "definition":{
                    "resource":[{
                        "reference":{"reference":"ValueSet/renamed-status"},
                        "name":"Renamed status values"
                    }],
                    "page":{
                        "nameUrl":"toc.html",
                        "title":"Table of Contents",
                        "generation":"html",
                        "page":[
                            {
                                "nameUrl":"artifacts.html",
                                "title":"Artifacts Summary",
                                "generation":"html"
                            },
                            {
                                "nameUrl":"conformance-patients.html",
                                "title":"Conformance: Patients",
                                "generation":"markdown"
                            }
                        ]
                    }
                }
                }),
            ),
        ];
        let prepared = engine
            .prepare_site(
                r#"{"generator":"publisher","templateCoordinate":"demo.template#1.0.0","buildEpochSecs":1,"activeTables":true}"#,
            )
            .unwrap();
        assert_eq!(
            prepared
                .site_build
                .site_build()
                .render_target()
                .template
                .as_ref(),
            Some(&site_build::PackageCoordinate::parse("demo.template#1.0.0").unwrap())
        );
        assert!(!prepared
            .site_build
            .site_build()
            .render_target()
            .parameters
            .contains_key("templateCoordinate"));
        let wire = serde_json::to_value(&prepared).unwrap();
        let timings = wire["metrics"].as_object().unwrap();
        let phase_names = [
            "projectRevisionMs",
            "packageLockMs",
            "preparedGuideKeyMs",
            "preparedGuideMs",
            "templateMaterializeMs",
            "publisherRuntimeMs",
            "publisherModelMs",
            "renderModelMs",
            "catalogMs",
        ];
        for name in phase_names {
            let phase = timings[name].as_f64().unwrap();
            assert!(phase >= 0.0, "{name} must be nonnegative");
            assert!(
                timings["totalMs"].as_f64().unwrap() >= phase,
                "total must include {name}"
            );
        }
        let phase_total = phase_names
            .iter()
            .map(|name| timings[*name].as_f64().unwrap())
            .sum::<f64>();
        assert!(
            timings["totalMs"].as_f64().unwrap() + f64::EPSILON >= phase_total,
            "timing phases must be disjoint: {timings:?}"
        );
        assert_eq!(timings["preparedGuideCacheHit"], false);
        assert_eq!(timings["siteBuildCacheHit"], false);
        let guide = &engine.prepared_guide_cache.as_ref().unwrap().guide;
        assert!(guide.pages.iter().any(|page| {
            page.name_url == "artifacts.html" && page.generation == "html" && page.body.is_none()
        }));
        assert!(guide
            .pages
            .iter()
            .any(|page| page.name_url == "conformance-patients.html"));
        assert!(!guide
            .authored_files
            .iter()
            .any(|asset| asset.path.as_str() == "patients-with-cancer-condition.svg"));
        let catalog = engine.site_outputs(&prepared.handle).unwrap();
        let paths = catalog
            .outputs
            .iter()
            .map(|output| output.path.as_str())
            .collect::<BTreeSet<_>>();
        assert!(paths.contains("en/index.html"));
        assert!(paths.contains("en/conformance-patients.html"));
        assert!(paths.contains("assets/app.css"));
        assert!(paths.contains("logo.svg"));
        assert!(paths.contains("icon_element.gif"));
        assert!(paths.contains("en/assets/app.css"));
        assert!(paths.contains("en/icon_element.gif"));
        let subject_page = catalog
            .outputs
            .iter()
            .find(|output| output.path.as_str() == "en/published-renamed-status-landing.html")
            .unwrap_or_else(|| panic!("configured ValueSet landing page; paths={paths:?}"));
        assert_eq!(subject_page.title.as_deref(), Some("RenamedStatus"));
        assert_eq!(
            subject_page.subject,
            Some(OutputResourceSubject {
                resource_type: "ValueSet".into(),
                id: "renamed-status".into(),
            })
        );
        assert_eq!(subject_page.subject_page, Some(OutputSubjectPage::Primary));
        let content_at = |path: &str| {
            catalog
                .outputs
                .iter()
                .find(|output| output.path.as_str() == path)
                .and_then(|output| output.content.clone())
                .unwrap()
        };
        assert_eq!(
            content_at("assets/app.css"),
            content_at("en/assets/app.css")
        );
        assert_eq!(
            content_at("icon_element.gif"),
            content_at("en/icon_element.gif")
        );
        let page = engine
            .render_site_output(&prepared.handle, "en/index.html")
            .unwrap();
        page.content
            .verify(
                &engine
                    .read_site_content(&prepared.handle, page.content.sha256.as_str())
                    .unwrap(),
            )
            .unwrap();
        let page_bytes = engine
            .read_site_content(&prepared.handle, page.content.sha256.as_str())
            .unwrap();
        let page_html = String::from_utf8(page_bytes).unwrap();
        for relative in ["assets/app.css", "icon_element.gif"] {
            assert!(page_html.contains(relative));
            assert!(paths.contains(format!("en/{relative}").as_str()));
        }

        let retained_state = match engine.site_builds.get(&prepared.handle).unwrap() {
            SiteBuildRuntime::Publisher(runtime) => runtime.state.clone(),
            SiteBuildRuntime::Cycle(_) => panic!("Publisher prepare retained Cycle runtime"),
        };
        let cycle_build = closed("facade-cycle-switch.test");
        let cycle_handle = cycle_build.site_build().build_id().to_string();
        engine.retain_site_build(
            cycle_handle.clone(),
            SiteBuildRuntime::Cycle(CycleBuildRuntime {
                build: cycle_build,
                objects: BTreeMap::new(),
            }),
        );
        let repeated = engine
            .prepare_site(
                r#"{"generator":"publisher","templateCoordinate":"demo.template#1.0.0","buildEpochSecs":1,"activeTables":true}"#,
            )
            .unwrap();
        assert_eq!(repeated.handle, prepared.handle);
        assert!(repeated.metrics.prepared_guide_cache_hit);
        assert!(repeated.metrics.snapshot_completed_local_cache_hit);
        assert!(repeated.metrics.site_build_cache_hit);
        assert_eq!(repeated.metrics.prepared_guide_ms, 0.0);
        assert_eq!(repeated.metrics.template_materialize_ms, 0.0);
        assert_eq!(repeated.metrics.publisher_runtime_ms, 0.0);
        assert_eq!(repeated.metrics.publisher_model_ms, 0.0);
        assert_eq!(repeated.metrics.render_model_ms, 0.0);
        assert_eq!(repeated.metrics.catalog_ms, 0.0);
        assert_eq!(engine.derived_cache_hits.retained_publisher_runtime, 1);
        assert_eq!(
            engine
                .site_build_generations
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![cycle_handle, prepared.handle.clone()]
        );
        let SiteBuildRuntime::Publisher(retained) =
            engine.site_builds.get(&prepared.handle).unwrap()
        else {
            panic!("retained Publisher runtime changed generator");
        };
        assert!(Rc::ptr_eq(&retained.state, &retained_state));
        assert!(retained
            .ready
            .contains_key(&site_build::OutputPath::parse("en/index.html").unwrap()));

        // A prose-only successor is a new complete PreparedGuide/SiteBuild,
        // but its compiled local resources and exact package context are
        // unchanged. Reuse only the snapshot-completed local set, then rerun
        // authored augmentation against the current bytes.
        engine.last_project.as_mut().unwrap().site_files.insert(
            "input/pagecontent/index.md".into(),
            base64_encode(b"# Updated intro"),
        );
        let prose_successor = engine
            .prepare_site(
                r#"{"generator":"publisher","templateCoordinate":"demo.template#1.0.0","buildEpochSecs":1,"activeTables":true}"#,
            )
            .unwrap();
        assert!(!prose_successor.metrics.prepared_guide_cache_hit);
        assert!(prose_successor.metrics.snapshot_completed_local_cache_hit);
        assert_eq!(engine.derived_cache_hits.snapshot_completed_local, 1);
        let guide = &engine.prepared_guide_cache.as_ref().unwrap().guide;
        assert!(guide.authored_files.iter().any(|file| {
            file.role == site_build::AuthoredFileRole::PageContent
                && file.path.as_str() == "index.md"
                && file.content == b"# Updated intro"
        }));
        assert_ne!(prose_successor.build_id, prepared.build_id);
    }
}

#[cfg(test)]
mod site_build_handoff_tests {
    use super::*;

    fn package_bundle(label: &str, marker: &str) -> serde_json::Value {
        let (name, version) = label.split_once('#').unwrap();
        serde_json::json!({
            "label": label,
            "files": {
                "package.json": base64_encode(
                    serde_json::to_string(&serde_json::json!({
                        "name": name,
                        "version": version,
                    }))
                    .unwrap()
                    .as_bytes()
                ),
                "marker.txt": base64_encode(marker.as_bytes()),
            }
        })
    }

    fn package_bundle_with_sd(label: &str, marker: &str, resource_version: &str) -> Value {
        let mut bundle = package_bundle(label, marker);
        let files = bundle["files"].as_object_mut().unwrap();
        let sd = serde_json::json!({
            "resourceType": "StructureDefinition",
            "id": "conflict",
            "url": "https://example.org/StructureDefinition/conflict",
            "version": resource_version,
            "name": "Conflict",
            "status": "draft",
            "kind": "resource",
            "abstract": false,
            "type": "Patient",
            "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Patient",
            "derivation": "constraint",
            "differential": { "element": [{ "id": "Patient", "path": "Patient" }] }
        });
        files.insert(
            "StructureDefinition-conflict.json".into(),
            Value::String(base64_encode(
                serde_json::to_string(&sd).unwrap().as_bytes(),
            )),
        );
        files.insert(
            ".index.json".into(),
            Value::String(base64_encode(
                serde_json::to_string(&serde_json::json!({
                    "index-version": 2,
                    "files": [{
                        "filename": "StructureDefinition-conflict.json",
                        "resourceType": "StructureDefinition",
                        "id": "conflict",
                        "url": "https://example.org/StructureDefinition/conflict",
                        "version": resource_version,
                        "kind": "resource",
                        "type": "Patient"
                    }]
                }))
                .unwrap()
                .as_bytes(),
            )),
        );
        bundle
    }

    #[test]
    fn wasm_mount_rejects_package_identity_and_dependency_shape_mismatches() {
        let mut engine = Engine::default();
        let mismatch = r#"[{"label":"example.pkg#1.0.0","files":{"package.json":"eyJuYW1lIjoid3JvbmcucGtnIiwidmVyc2lvbiI6IjEuMC4wIn0="}}]"#;
        assert!(engine
            .init(mismatch)
            .unwrap_err()
            .contains("disagrees with package.json"));
        assert!(
            engine.bundle.is_none(),
            "failed init must not commit a bundle"
        );

        let malformed_dependencies = r#"[{"label":"example.pkg#1.0.0","files":{"package.json":"eyJuYW1lIjoiZXhhbXBsZS5wa2ciLCJ2ZXJzaW9uIjoiMS4wLjAiLCJkZXBlbmRlbmNpZXMiOltdfQ=="}}]"#;
        assert!(engine
            .init(malformed_dependencies)
            .unwrap_err()
            .contains("dependencies must map ids to strings"));
    }

    #[test]
    fn prepared_mount_is_validated_idempotent_and_uses_shipped_index() {
        let mut engine = Engine::default();
        engine.init("[]").unwrap();
        let prepared = package_store::PreparedPackage::prepare(
            "example.pkg#1.0.0",
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"example.pkg","version":"1.0.0"}"#.to_vec(),
                ),
                (
                    "StructureDefinition-p.json".into(),
                    br#"{"resourceType":"StructureDefinition","id":"p","name":"P"}"#.to_vec(),
                ),
            ]),
        )
        .unwrap();
        let bytes = prepared.encode();
        let key = prepared.key.to_string();
        assert_eq!(engine.mount_prepared(bytes.clone(), &key).unwrap(), 1);
        assert_eq!(engine.mount_prepared(bytes.clone(), &key).unwrap(), 1);
        let source = engine.bundle.as_ref().unwrap();
        let sidecar = source
            .read(
                &source
                    .cache_root()
                    .join("example.pkg#1.0.0/package/.derived-index.json"),
            )
            .unwrap();
        assert!(package_store::derived_index::parse(&sidecar).is_some());

        let mut corrupted = bytes;
        corrupted[10] ^= 1;
        assert!(engine
            .mount_prepared(corrupted, &key)
            .unwrap_err()
            .contains("checksum"));
        assert_eq!(engine.packages, vec!["example.pkg#1.0.0"]);
    }

    #[test]
    fn staged_prepared_mount_bounds_host_batches_and_commits_atomically() {
        let package = |label: &str| {
            let (name, version) = label.split_once('#').unwrap();
            package_store::PreparedPackage::prepare(
                label,
                BTreeMap::from([
                    (
                        "package.json".into(),
                        serde_json::to_vec(&serde_json::json!({
                            "name": name,
                            "version": version,
                        }))
                        .unwrap(),
                    ),
                    ("marker.txt".into(), label.as_bytes().to_vec()),
                ]),
            )
            .unwrap()
        };
        let a = package("a.pkg#1.0.0");
        let b = package("b.pkg#1.0.0");
        let a_bytes = a.encode();
        let b_bytes = b.encode();
        let mut engine = Engine::default();
        engine.init("[]").unwrap();

        engine.begin_prepared_mount(2).unwrap();
        let staged = engine
            .stage_prepared_mount(a_bytes.clone(), &a.key.cache_key())
            .unwrap();
        assert_eq!(staged.staged, 1);
        assert!(
            engine.packages.is_empty(),
            "staging must not mutate the engine"
        );
        let mut corrupt = b_bytes.clone();
        corrupt[10] ^= 1;
        assert!(engine
            .stage_prepared_mount(corrupt, &b.key.cache_key())
            .is_err());
        assert!(engine.abort_prepared_mount());
        assert!(engine.packages.is_empty());

        engine.begin_prepared_mount(2).unwrap();
        engine
            .stage_prepared_mount(a_bytes.clone(), &a.key.cache_key())
            .unwrap();
        engine
            .stage_prepared_mount(b_bytes.clone(), &b.key.cache_key())
            .unwrap();
        let outcome = engine.commit_prepared_mount().unwrap();
        assert_eq!(outcome.added, 2);
        assert_eq!(outcome.packages, 2);
        assert_eq!(
            outcome.artifact_bytes,
            (a_bytes.len() + b_bytes.len()) as u64
        );
        assert_eq!(outcome.retained_blob_bytes, outcome.artifact_bytes);
        assert_eq!(outcome.member_body_copies, 0);
        assert_eq!(engine.packages, vec!["a.pkg#1.0.0", "b.pkg#1.0.0"]);
    }

    #[test]
    fn cold_prepare_mount_exports_direct_binary_once() {
        let mut engine = Engine::default();
        engine.init("[]").unwrap();
        let input =
            serde_json::to_string(&vec![package_bundle("example.pkg#1.0.0", "cold")]).unwrap();
        let outcome = engine.prepare_and_mount(&input).unwrap();
        assert_eq!(outcome.added, 1);
        assert_eq!(outcome.artifacts.len(), 1);
        assert_eq!(outcome.mount_member_body_copies, 0);
        assert!(outcome.prepared_members >= 2);
        assert_eq!(outcome.input_json_bytes, input.len() as u64);
        assert!(outcome.base64_bytes > outcome.decoded_source_bytes);
        assert!(outcome.normalized_bytes >= outcome.decoded_source_bytes);
        assert!(outcome.json_parse_ms >= 0.0);
        assert!(outcome.base64_decode_ms >= 0.0);
        assert!(outcome.normalization_ms >= 0.0);
        assert!(outcome.indexing_ms >= 0.0);
        assert!(outcome.artifact_encode_ms >= 0.0);
        let export = &outcome.artifacts[0];
        let bytes = engine.take_prepared(&export.label).unwrap();
        assert_eq!(bytes.len() as u64, export.bytes);
        assert_eq!(
            site_build::Sha256Digest::of_bytes(&bytes).to_string(),
            export.artifact_sha256
        );
        let key: package_store::PreparedPackageKey = export.cache_key.parse().unwrap();
        package_store::PreparedPackage::decode_expected(&bytes, &key).unwrap();
        assert!(engine.take_prepared(&export.label).is_err());
    }

    #[test]
    fn duplicate_init_labels_fail_transactionally_instead_of_overlaying_bytes() {
        let package_json = "eyJuYW1lIjoiZXhhbXBsZS5wa2ciLCJ2ZXJzaW9uIjoiMS4wLjAifQ==";
        let mut engine = Engine::default();
        engine
            .init(&format!(
                r#"[{{"label":"example.pkg#1.0.0","files":{{"package.json":"{package_json}","First.json":"e30="}}}}]"#,
            ))
            .unwrap();
        let before = engine.package_materials["example.pkg#1.0.0"]
            .content
            .clone();

        let duplicate = format!(
            r#"[
              {{"label":"example.pkg#1.0.0","files":{{"package.json":"{package_json}","First.json":"e30="}}}},
              {{"label":"example.pkg#1.0.0","files":{{"package.json":"{package_json}","Second.json":"e30="}}}}
            ]"#,
        );
        assert!(engine
            .init(&duplicate)
            .unwrap_err()
            .contains("duplicate package label"));

        assert_eq!(engine.packages, vec!["example.pkg#1.0.0"]);
        assert_eq!(
            engine.package_materials["example.pkg#1.0.0"].content,
            before
        );
        let source = engine.bundle.as_ref().unwrap();
        let package_dir = source
            .cache_root()
            .join("example.pkg#1.0.0")
            .join("package");
        assert_eq!(source.read(&package_dir.join("First.json")).unwrap(), b"{}");
        assert!(source.read(&package_dir.join("Second.json")).is_err());
    }

    #[test]
    fn duplicate_new_mount_labels_fail_instead_of_silently_first_winning() {
        let mut engine = Engine::default();
        engine
            .init(
                r#"[{"label":"example.pkg#1.0.0","files":{"package.json":"eyJuYW1lIjoiZXhhbXBsZS5wa2ciLCJ2ZXJzaW9uIjoiMS4wLjAifQ=="}}]"#,
            )
            .unwrap();
        let duplicate = r#"[
          {"label":"other.pkg#1.0.0","files":{"package.json":"eyJuYW1lIjoib3RoZXIucGtnIiwidmVyc2lvbiI6IjEuMC4wIn0=","First.json":"e30="}},
          {"label":"other.pkg#1.0.0","files":{"package.json":"eyJuYW1lIjoib3RoZXIucGtnIiwidmVyc2lvbiI6IjEuMC4wIn0=","Second.json":"e30="}}
        ]"#;
        assert!(engine
            .mount(duplicate)
            .unwrap_err()
            .contains("duplicate new package label"));
        assert_eq!(engine.packages, vec!["example.pkg#1.0.0"]);
        assert!(!engine.package_materials.contains_key("other.pkg#1.0.0"));
    }

    #[test]
    fn post_compile_mount_invalidates_next_resolution_but_current_revision_stays_scoped() {
        let first = "example.pkg#1.0.0";
        let second = "example.pkg#2.0.0";
        let mut engine = Engine::default();
        engine
            .init(
                &serde_json::to_string(&vec![package_bundle_with_sd(first, "first", "1.0.0")])
                    .unwrap(),
            )
            .unwrap();
        let selected = ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(b"config"),
            resolution_support: BTreeSet::new(),
            labels: vec![first.into()],
        };
        engine.resolved_packages = Some(selected.clone());
        engine.last_project = Some(CompiledProjectRevision {
            config: "config".into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::new(),
            resolved_packages: Some(selected),
        });
        engine.last_compiled = vec![(
            PathBuf::from("/__ig__/ImplementationGuide-primary.json"),
            serde_json::json!({
                "resourceType": "ImplementationGuide",
                "id": "primary",
                "packageId": "example.primary",
                "url": "https://example.org/ImplementationGuide/primary",
                "version": "1.0.0",
                "fhirVersion": ["4.0.1"]
            }),
        )];
        engine
            .mount(
                &serde_json::to_string(&vec![package_bundle_with_sd(second, "second", "2.0.0")])
                    .unwrap(),
            )
            .unwrap();
        assert!(engine.resolved_packages.is_none());

        // Every operation derived from the current revision uses this scoped
        // package view rather than the newly enlarged cache.
        let (source, root, labels) = engine.source_for_current_revision().unwrap();
        assert_eq!(labels, vec![first]);
        let listed = source.read_dir(&root).unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|entry| entry.file_name.as_str())
                .collect::<Vec<_>>(),
            vec![first]
        );
        assert_eq!(
            source
                .read(&root.join(first).join("package/marker.txt"))
                .unwrap(),
            b"first"
        );
        assert!(source
            .read(&root.join(second).join("package/marker.txt"))
            .is_err());

        let context = engine.build_context().unwrap();
        let resolved = context
            .fetch("https://example.org/StructureDefinition/conflict")
            .unwrap();
        assert_eq!(resolved["version"], "1.0.0");
    }

    #[test]
    fn wasm_package_boundary_retains_safe_nested_template_content() {
        let mut engine = Engine::default();
        engine
            .init(
                r#"[{
                  "label":"demo.template#1.0.0",
                  "files":{
                    "package.json":"eyJuYW1lIjoiZGVtby50ZW1wbGF0ZSIsInZlcnNpb24iOiIxLjAuMCIsInR5cGUiOiJmaGlyLnRlbXBsYXRlIn0=",
                    "config.json":"eyJmb3JtYXRzIjpbXX0=",
                    "includes/demo.html":"bmVzdGVkIGluY2x1ZGU=",
                    "layouts/default.html":"bGF5b3V0"
                  }
                }]"#,
            )
            .unwrap();

        let (source, root, _) = engine.source().unwrap();
        let tree = package_store::template_loader::materialize(
            &source,
            &package_store::template_loader::TemplatePaths::new(&root),
            "demo.template#1.0.0",
        )
        .unwrap();
        assert_eq!(tree.get("includes/demo.html"), Some(&b"nested include"[..]));
        assert_eq!(tree.get("layouts/default.html"), Some(&b"layout"[..]));
    }

    #[test]
    fn project_revision_covers_raw_site_bytes_and_is_deterministic() {
        let engine = Engine::default();
        let project = CompiledProjectRevision {
            config: "id: demo\nfhirVersion: 4.0.1\n".into(),
            fsh: BTreeMap::from([("input/fsh/demo.fsh".into(), "Profile: Demo".into())]),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::from([("input/images/x.txt".into(), "eA==".into())]),
            resolved_packages: None,
        };
        let first = engine
            .site_build_project_revision(&project, "demo.ig")
            .unwrap();
        let second = engine
            .site_build_project_revision(&project, "demo.ig")
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(first.sources.iter().count(), 3);
        assert!(first.revision.starts_with("sources-sha256:"));

        let mut config_collision = project.clone();
        config_collision
            .fsh
            .insert("sushi-config.yaml".into(), "Alias: Bad".into());
        assert!(engine
            .site_build_project_revision(&config_collision, "demo.ig")
            .unwrap_err()
            .contains("more than one input channel"));

        let mut channel_collision = project;
        channel_collision
            .fsh
            .insert("input/images/x.txt".into(), "Profile: AlsoBad".into());
        assert!(engine
            .site_build_project_revision(&channel_collision, "demo.ig")
            .unwrap_err()
            .contains("more than one input channel"));

        let resource_path = "input/resources/Patient-p.json";
        let parsed = serde_json::json!({"resourceType": "Patient", "id": "p"});
        let equivalent = CompiledProjectRevision {
            config: "id: demo".into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::from([(resource_path.into(), parsed.clone())]),
            site_files: BTreeMap::from([(
                resource_path.into(),
                base64_encode(b"{ \"id\": \"p\", \"resourceType\": \"Patient\" }"),
            )]),
            resolved_packages: None,
        };
        engine
            .site_build_project_revision(&equivalent, "demo.ig")
            .unwrap();
        let mut different = equivalent;
        different.site_files.insert(
            resource_path.into(),
            base64_encode(br#"{"resourceType":"Patient","id":"other"}"#),
        );
        assert!(engine
            .site_build_project_revision(&different, "demo.ig")
            .unwrap_err()
            .contains("differs from the raw authored site file"));

        let mut only_predefined = different.clone();
        only_predefined.site_files.clear();
        assert!(engine
            .site_build_project_revision(&only_predefined, "demo.ig")
            .unwrap_err()
            .contains("only parsed"));
        let mut only_raw = different;
        only_raw.predefined.clear();
        assert!(engine
            .site_build_project_revision(&only_raw, "demo.ig")
            .unwrap_err()
            .contains("only raw"));
    }

    #[test]
    fn package_lock_is_bound_to_resolved_config_and_intersects_declared_graph() {
        let config = "id: demo\nfhirVersion: 4.0.1\n";
        let core = "hl7.fhir.r4.core#4.0.1";
        let dep = "example.dep#1.0.0";
        let resolved = ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support: BTreeSet::new(),
            labels: vec![core.into(), dep.into()],
        };
        let engine = Engine {
            package_materials: BTreeMap::from([
                (
                    core.into(),
                    MountedPackage {
                        content: site_build::ContentRef::of_bytes(b"core", None::<String>),
                        declared_dependencies: BTreeMap::new(),
                    },
                ),
                (
                    dep.into(),
                    MountedPackage {
                        content: site_build::ContentRef::of_bytes(b"dep", None::<String>),
                        declared_dependencies: BTreeMap::from([
                            ("hl7.fhir.r4.core".into(), "4.0.1".into()),
                            // Not in the resolver's exact closure, so it is not
                            // falsely claimed as a locked/read package.
                            ("unused.optional".into(), "1.0.0".into()),
                        ]),
                    },
                ),
            ]),
            // A later same-config resolution must not replace the closure that
            // was captured by the compiled project below.
            resolved_packages: Some(ResolvedPackages {
                config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
                resolution_support: BTreeSet::new(),
                labels: vec![core.into(), "example.dep#2.0.0".into()],
            }),
            ..Default::default()
        };
        let project = CompiledProjectRevision {
            config: config.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::new(),
            resolved_packages: Some(resolved.clone()),
        };
        let lock = engine.site_build_package_lock(&project).unwrap();
        let dependency = lock
            .get(&site_build::PackageCoordinate::parse(dep).unwrap())
            .unwrap();
        assert_eq!(dependency.dependencies.len(), 1);
        let mut wrong = project;
        wrong.config = "id: different".into();
        assert!(engine
            .site_build_package_lock(&wrong)
            .unwrap_err()
            .contains("different config"));
    }

    #[test]
    fn package_lock_preserves_two_resolved_versions_and_exact_dependency_edges() {
        let config = "id: demo\nfhirVersion: 4.0.1\n";
        let old = "hl7.terminology.r4#7.1.0";
        let new = "hl7.terminology.r4#7.2.0";
        let consumer = "example.consumer#1.0.0";
        let material = |bytes: &'static [u8], dependencies| MountedPackage {
            content: site_build::ContentRef::of_bytes(bytes, None::<String>),
            declared_dependencies: dependencies,
        };
        let resolved = ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support: BTreeSet::new(),
            labels: vec![old.into(), new.into(), consumer.into()],
        };
        let engine = Engine {
            package_materials: BTreeMap::from([
                (old.into(), material(b"old", BTreeMap::new())),
                (new.into(), material(b"new", BTreeMap::new())),
                (
                    consumer.into(),
                    material(
                        b"consumer",
                        BTreeMap::from([("hl7.terminology.r4".into(), "7.1.0".into())]),
                    ),
                ),
            ]),
            ..Default::default()
        };
        let project = CompiledProjectRevision {
            config: config.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::new(),
            resolved_packages: Some(resolved),
        };

        let lock = engine.site_build_package_lock(&project).unwrap();
        assert!(lock
            .get(&site_build::PackageCoordinate::parse(old).unwrap())
            .is_some());
        assert!(lock
            .get(&site_build::PackageCoordinate::parse(new).unwrap())
            .is_some());
        let dependency = lock
            .get(&site_build::PackageCoordinate::parse(consumer).unwrap())
            .unwrap();
        assert_eq!(
            dependency.dependencies,
            BTreeSet::from([site_build::PackageCoordinate::parse(old).unwrap()])
        );
    }

    #[test]
    fn package_lock_omits_proven_filtered_exact_edge_without_retargeting_it() {
        let config = "id: mcode-like\nfhirVersion: 4.0.1\n";
        let extensions_old = "hl7.fhir.uv.extensions.r4#1.0.0";
        let extensions_current = "hl7.fhir.uv.extensions.r4#5.3.0";
        let extensions_filtered = "hl7.fhir.uv.extensions.r4#5.2.0";
        let consumer = "example.mcode-like#4.0.0";
        let material = |bytes: &'static [u8], dependencies| MountedPackage {
            content: site_build::ContentRef::of_bytes(bytes, None::<String>),
            declared_dependencies: dependencies,
        };
        let request = |label: &str| {
            let (package_id, version) = label.split_once('#').unwrap();
            package_store::PackageRequest {
                package_id: package_id.into(),
                version: version.into(),
            }
        };
        let resolved = resolved_packages_from_step(
            config,
            &package_store::ResolutionStep {
                resolver_schema: package_store::RESOLVER_SCHEMA,
                // The executable closure independently contains two other
                // exact versions of the same package id.
                compile_set: vec![
                    request(extensions_old),
                    request(extensions_current),
                    request(consumer),
                ],
                context_closure: Vec::new(),
                // The resolver read 5.2.0 and rejected it at its R4
                // compatibility filter.
                resolution_support: vec![request(extensions_filtered)],
                missing: Vec::new(),
                satisfied: true,
                mutable_requests: Vec::new(),
            },
        )
        .unwrap();
        let engine = Engine {
            package_materials: BTreeMap::from([
                (
                    extensions_old.into(),
                    material(b"extensions-old", BTreeMap::new()),
                ),
                (
                    extensions_current.into(),
                    material(b"extensions-current", BTreeMap::new()),
                ),
                (
                    extensions_filtered.into(),
                    material(b"extensions-filtered", BTreeMap::new()),
                ),
                (
                    consumer.into(),
                    material(
                        b"consumer",
                        BTreeMap::from([("hl7.fhir.uv.extensions.r4".into(), "5.2.0".into())]),
                    ),
                ),
            ]),
            ..Default::default()
        };
        let project = |resolved_packages| CompiledProjectRevision {
            config: config.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::new(),
            resolved_packages: Some(resolved_packages),
        };

        let lock = engine
            .site_build_package_lock(&project(resolved.clone()))
            .unwrap();
        let consumer = lock
            .get(&site_build::PackageCoordinate::parse(consumer).unwrap())
            .unwrap();
        assert!(
            consumer.dependencies.is_empty(),
            "the filtered 5.2.0 edge must not be retargeted to 5.3.0 or 1.0.0"
        );
        lock.iter().for_each(|(_, package)| {
            package.dependencies.iter().for_each(|dependency| {
                assert!(
                    lock.get(dependency).is_some(),
                    "lock emitted a dangling edge"
                );
            });
        });

        // Without the resolver's exact exclusion witness, the same version
        // mismatch is a real closure inconsistency and must still fail loudly.
        let mut unproven = resolved;
        unproven.resolution_support.clear();
        let error = engine
            .site_build_package_lock(&project(unproven))
            .unwrap_err();
        assert!(
            error.contains("matches neither the resolved coordinates"),
            "{error}"
        );
    }

    #[test]
    fn cycle_zero_dependency_project_resolves_exact_automatic_transitives_before_prepare() {
        let bundle = |package_id: &str, version: &str, dependencies: BTreeMap<&str, &str>| {
            let label = format!("{package_id}#{version}");
            let manifest = serde_json::json!({
                "name": package_id,
                "version": version,
                "fhirVersions": ["4.0.1"],
                "dependencies": dependencies,
            });
            serde_json::json!({
                "label": label,
                "files": {
                    "package.json": base64_encode(manifest.to_string().as_bytes()),
                }
            })
        };
        let current = vec![
            bundle(
                "hl7.fhir.uv.tools.r4",
                "1.1.2",
                BTreeMap::from([
                    ("hl7.fhir.r4.core", "4.0.1"),
                    ("hl7.terminology.r4", "7.1.0"),
                    ("hl7.fhir.uv.extensions.r4", "5.2.0"),
                ]),
            ),
            bundle("hl7.terminology.r4", "7.2.0", BTreeMap::new()),
            bundle("hl7.fhir.r4.core", "4.0.1", BTreeMap::new()),
            bundle("hl7.fhir.uv.extensions.r4", "5.3.0", BTreeMap::new()),
        ];
        let exact_transitives = vec![
            bundle(
                "hl7.terminology.r4",
                "7.1.0",
                BTreeMap::from([("hl7.fhir.r4.core", "4.0.1")]),
            ),
            bundle(
                "hl7.fhir.uv.extensions.r4",
                "5.2.0",
                BTreeMap::from([("hl7.fhir.r4.core", "4.0.1")]),
            ),
        ];
        let config = "id: cycle.test\ncanonical: https://example.org/cycle\nname: CycleTest\nstatus: draft\nversion: 1.0.0\nfhirVersion: 4.0.1\n";
        let index = serde_json::json!({
            "versions": {
                "hl7.fhir.uv.tools.r4": ["1.1.2"],
                "hl7.terminology.r4": ["7.2.0"],
                "hl7.fhir.r4.core": ["4.0.1"],
                "hl7.fhir.uv.extensions.r4": ["5.3.0"]
            }
        })
        .to_string();

        let mut engine = Engine::default();
        engine
            .init(&serde_json::to_string(&current).unwrap())
            .unwrap();
        let incomplete: package_store::ResolutionStep =
            serde_json::from_str(&engine.resolve_project(config, &index).unwrap()).unwrap();
        assert!(!incomplete.satisfied);
        assert!(incomplete.missing.iter().any(|missing| {
            missing.package_id == "hl7.fhir.uv.extensions.r4" && missing.version == "5.2.0"
        }));

        engine
            .mount(&serde_json::to_string(&exact_transitives).unwrap())
            .unwrap();
        let complete: package_store::ResolutionStep =
            serde_json::from_str(&engine.resolve_project(config, &index).unwrap()).unwrap();
        assert!(complete.satisfied, "missing={:?}", complete.missing);
        assert!(complete.context_closure.iter().any(|request| {
            request.package_id == "hl7.fhir.uv.extensions.r4" && request.version == "5.2.0"
        }));

        engine.compile_project("{}", config, "{}", "{}").unwrap();
        let prepared = engine
            .prepare_site(r#"{"generator":"cycle","buildEpochSecs":1}"#)
            .unwrap();
        let lock = prepared.site_build.site_build().package_lock();
        let tools = site_build::PackageCoordinate::parse("hl7.fhir.uv.tools.r4#1.1.2").unwrap();
        let extensions_old =
            site_build::PackageCoordinate::parse("hl7.fhir.uv.extensions.r4#5.2.0").unwrap();
        let extensions_current =
            site_build::PackageCoordinate::parse("hl7.fhir.uv.extensions.r4#5.3.0").unwrap();
        assert!(lock.get(&extensions_old).is_some());
        assert!(lock.get(&extensions_current).is_some());
        assert_eq!(
            lock.get(&tools).unwrap().dependencies,
            BTreeSet::from([
                site_build::PackageCoordinate::parse("hl7.fhir.r4.core#4.0.1").unwrap(),
                site_build::PackageCoordinate::parse("hl7.terminology.r4#7.1.0").unwrap(),
                extensions_old,
            ])
        );
    }

    #[test]
    fn core_selection_uses_the_exact_resolved_closure_not_mount_order() {
        let config = "id: demo\nfhirVersion: 4.0.1\n";
        let selected = "hl7.fhir.r4.core#4.0.1";
        let unrelated_r4 = "hl7.fhir.r4.core#4.0.0";
        let unrelated_r5 = "hl7.fhir.r5.core#5.0.0";
        let material = || MountedPackage {
            content: site_build::ContentRef::of_bytes(b"core", None::<String>),
            declared_dependencies: BTreeMap::new(),
        };
        let resolved = ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support: BTreeSet::new(),
            labels: vec![selected.into()],
        };
        let engine = Engine {
            packages: vec![unrelated_r4.into(), unrelated_r5.into(), selected.into()],
            package_materials: BTreeMap::from([
                (unrelated_r4.into(), material()),
                (unrelated_r5.into(), material()),
                (selected.into(), material()),
            ]),
            ..Default::default()
        };

        assert_eq!(
            engine
                .resolved_core_package(&resolved, config, "4.0.1", "test")
                .unwrap(),
            selected
        );
        assert!(engine
            .resolved_core_package(&resolved, config, "5.0.0", "test")
            .unwrap_err()
            .contains("resolved closure has no required core package"));
        assert!(engine
            .resolved_core_package(&resolved, "id: other", "4.0.1", "test")
            .unwrap_err()
            .contains("different config"));
    }

    #[test]
    fn publisher_target_core_selection_ignores_support_cores_and_fails_closed() {
        let lock = |coordinates: &[&str]| {
            site_build::PackageLock::from_packages(coordinates.iter().map(|coordinate| {
                let coordinate = site_build::PackageCoordinate::parse(*coordinate).unwrap();
                site_build::LockedPackage {
                    content: site_build::ContentRef::of_bytes(
                        coordinate.to_string().as_bytes(),
                        None::<String>,
                    ),
                    coordinate,
                    dependencies: BTreeSet::new(),
                }
            }))
            .unwrap()
        };

        let mixed = lock(&["hl7.fhir.r4.core#4.0.1", "hl7.fhir.r5.core#5.0.0"]);
        assert_eq!(
            target_core_from_package_lock(&mixed, "4.0.1", "test")
                .unwrap()
                .to_string(),
            "hl7.fhir.r4.core#4.0.1"
        );

        let missing = lock(&["hl7.fhir.r5.core#5.0.0"]);
        assert!(target_core_from_package_lock(&missing, "4.0.1", "test")
            .unwrap_err()
            .contains("no required target core hl7.fhir.r4.core#4.0.1"));

        let ambiguous = lock(&[
            "hl7.fhir.r4.core#4.0.0",
            "hl7.fhir.r4.core#4.0.1",
            "hl7.fhir.r5.core#5.0.0",
        ]);
        assert!(target_core_from_package_lock(&ambiguous, "4.0.1", "test")
            .unwrap_err()
            .contains("multiple coordinates for target core package hl7.fhir.r4.core"));
    }

    fn minimal_prepared_guide() -> site_build::PreparedGuide {
        let key = site_build::SemanticResourceKey {
            resource_type: "ImplementationGuide".into(),
            id: "demo".into(),
        };
        site_build::PreparedGuide {
            guide: site_build::GuideIdentity {
                implementation_guide: key.clone(),
                package_id: "demo.ig".into(),
                canonical: Some("https://example.org/demo".into()),
                name: Some("Demo".into()),
                version: Some("1.0.0".into()),
                fhir_version: "4.0.1".into(),
                release_label: None,
                fhir_publication_base: "http://hl7.org/fhir/R4/".into(),
                generated: site_build::GeneratedIdentity {
                    epoch_seconds: 1,
                    date: "1970-01-01T00:00:01Z".into(),
                    day: "19700101".into(),
                },
                source_control: None,
            },
            resources: vec![site_build::SemanticResource {
                key,
                resource: serde_json::json!({
                    "resourceType": "ImplementationGuide",
                    "id": "demo"
                }),
                publication: None,
            }],
            publisher_compatibility: None,
            expansions: Vec::new(),
            pages: Vec::new(),
            menu: Vec::new(),
            sushi_config: serde_json::json!({"id":"demo.ig"}),
            authored_files: Vec::new(),
        }
    }

    #[test]
    fn browser_examples_share_one_ordered_collision_free_local_resource_channel() {
        let resource_path = "input/resources/Patient-shared.json";
        let example_path = "input/examples/Patient-shared.json";
        let resources = BTreeMap::from([
            (
                example_path.into(),
                serde_json::json!({"resourceType":"Patient","id":"shared","marker":"example"}),
            ),
            (
                resource_path.into(),
                serde_json::json!({"resourceType":"Patient","id":"shared","marker":"resource"}),
            ),
        ]);
        let ordered = ordered_predefined_resources(&resources);
        assert_eq!(ordered[0].0, PathBuf::from(resource_path));
        assert_eq!(ordered[1].0, PathBuf::from(example_path));

        let resource_render_path = predefined_render_path(&ordered[0].0).unwrap();
        let example_render_path = predefined_render_path(&ordered[1].0).unwrap();
        assert_eq!(
            resource_render_path,
            PathBuf::from("/__predefined__/input/resources/Patient-shared.json")
        );
        assert_eq!(
            example_render_path,
            PathBuf::from("/__predefined__/input/examples/Patient-shared.json")
        );
        assert_ne!(resource_render_path, example_render_path);

        let primary = serde_json::json!({
            "resourceType":"ImplementationGuide", "id":"primary",
            "packageId":"example.primary", "fhirVersion":["4.0.1"],
            "definition":{"resource":[{
                "reference":{"reference":"Patient/shared"}, "exampleBoolean":true
            }]}
        });
        let mut render_set = ordered
            .into_iter()
            .map(|(path, body)| (predefined_render_path(&path).unwrap(), body))
            .collect::<Vec<_>>();
        render_set.push((
            PathBuf::from("/__ig__/ImplementationGuide-primary.json"),
            primary,
        ));
        let (selected, _) = prepared_local_resource_set(&render_set, "test").unwrap();
        assert_eq!(
            selected
                .iter()
                .find(|(_, body)| body["resourceType"] == "Patient")
                .unwrap()
                .1["marker"],
            "example",
            "stock resources-before-examples order must give the example later precedence"
        );

        let site_files = BTreeMap::from([
            (
                resource_path.into(),
                base64_encode(resources[resource_path].to_string().as_bytes()),
            ),
            (
                example_path.into(),
                base64_encode(resources[example_path].to_string().as_bytes()),
            ),
        ]);
        validate_predefined_site_overlap(&resources, &site_files, "test").unwrap();
        assert_eq!(
            source_kind_and_media_type(example_path).0,
            site_build::SourceKind::PredefinedResource
        );
    }

    #[test]
    fn prepared_local_set_deduplicates_and_preserves_explicit_primary_metadata() {
        let declarations = serde_json::json!([
            {"reference":{"reference":"Patient/example"},"exampleBoolean":true},
            {"reference":{"reference":"Observation/example"},"exampleCanonical":"https://example.org/StructureDefinition/example"}
        ]);
        let primary = serde_json::json!({
            "resourceType":"ImplementationGuide", "id":"primary",
            "packageId":"example.primary", "fhirVersion":["4.0.1"],
            "marker":"explicit", "definition":{"resource":declarations.clone()}
        });
        let render_set = vec![
            (
                PathBuf::from("/__compiled__/Patient-example.json"),
                serde_json::json!({"resourceType":"Patient","id":"example","marker":"compiled"}),
            ),
            (
                PathBuf::from("/__compiled__/ImplementationGuide-secondary.json"),
                serde_json::json!({"resourceType":"ImplementationGuide","id":"secondary"}),
            ),
            (
                PathBuf::from("/__predefined__/Patient-example.json"),
                serde_json::json!({"resourceType":"Patient","id":"example","marker":"predefined"}),
            ),
            (
                PathBuf::from("/__predefined__/ImplementationGuide-primary.json"),
                serde_json::json!({"resourceType":"ImplementationGuide","id":"primary","marker":"wrong"}),
            ),
            (
                PathBuf::from("/__ig__/ImplementationGuide-primary.json"),
                primary.clone(),
            ),
        ];

        let (resources, selected_primary) =
            prepared_local_resource_set(&render_set, "test").unwrap();
        assert_eq!(selected_primary, primary);
        assert_eq!(
            selected_primary.pointer("/definition/resource"),
            Some(&declarations)
        );
        assert_eq!(resources.len(), 3);
        assert_eq!(
            resources
                .iter()
                .find(|(_, value)| value["resourceType"] == "Patient")
                .unwrap()
                .1["marker"],
            "predefined"
        );
        assert_eq!(
            resources
                .iter()
                .find(|(_, value)| value["id"] == "primary")
                .unwrap()
                .1["marker"],
            "explicit"
        );
        assert!(resources
            .iter()
            .any(|(_, value)| value["id"] == "secondary"));

        let mut multiple = render_set.clone();
        multiple.push((
            PathBuf::from("/__ig__/ImplementationGuide-other.json"),
            serde_json::json!({"resourceType":"ImplementationGuide","id":"other"}),
        ));
        assert!(prepared_local_resource_set(&multiple, "test")
            .unwrap_err()
            .contains("multiple explicit primary"));
        assert!(prepared_local_resource_set(&render_set[..4], "test")
            .unwrap_err()
            .contains("no explicit primary"));
    }

    #[test]
    fn prepared_guide_snapshot_completes_predefined_structure_definition() {
        let base = serde_json::json!({
            "resourceType":"StructureDefinition", "id":"Patient",
            "url":"http://hl7.org/fhir/StructureDefinition/Patient", "version":"4.0.1",
            "name":"Patient", "status":"active", "kind":"resource", "abstract":false,
            "type":"Patient", "derivation":"specialization",
            "snapshot":{"element":[{"id":"Patient","path":"Patient","min":0,"max":"*"}]},
            "differential":{"element":[{"id":"Patient","path":"Patient","min":0,"max":"*"}]}
        });
        let bundle = serde_json::json!([{
            "label":"hl7.fhir.r4.core#4.0.1", "files":{
                "package.json":base64_encode(serde_json::json!({"name":"hl7.fhir.r4.core","version":"4.0.1","fhirVersions":["4.0.1"]}).to_string().as_bytes()),
                ".index.json":base64_encode(serde_json::json!({"index-version":2,"files":[{"filename":"StructureDefinition-Patient.json","resourceType":"StructureDefinition","id":"Patient","url":"http://hl7.org/fhir/StructureDefinition/Patient","version":"4.0.1","kind":"resource","type":"Patient"}]}).to_string().as_bytes()),
                "StructureDefinition-Patient.json":base64_encode(base.to_string().as_bytes())
            }
        }]);
        let config = "id: snapshot.test\ncanonical: https://example.org\nstatus: draft\nfhirVersion: 4.0.1\n";
        let path = "input/resources/StructureDefinition-local-profile.json";
        let differential = serde_json::json!({
            "resourceType":"StructureDefinition", "id":"local-profile",
            "url":"https://example.org/StructureDefinition/local-profile", "name":"LocalProfile",
            "status":"draft", "kind":"resource", "abstract":false, "type":"Patient",
            "baseDefinition":"http://hl7.org/fhir/StructureDefinition/Patient", "derivation":"constraint",
            "differential":{"element":[{"id":"Patient","path":"Patient","short":"Local profile"}]}
        });
        let primary = serde_json::json!({
            "resourceType":"ImplementationGuide", "id":"snapshot-test", "packageId":"snapshot.test",
            "url":"https://example.org/ImplementationGuide/snapshot-test", "fhirVersion":["4.0.1"],
            "definition":{"resource":[{"reference":{"reference":"StructureDefinition/local-profile"}}]}
        });
        let resolved = ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            resolution_support: BTreeSet::new(),
            labels: vec!["hl7.fhir.r4.core#4.0.1".into()],
        };
        let project = CompiledProjectRevision {
            config: config.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::from([(path.into(), differential.clone())]),
            site_files: BTreeMap::from([(
                path.into(),
                base64_encode(differential.to_string().as_bytes()),
            )]),
            resolved_packages: Some(resolved),
        };
        let mut engine = Engine::default();
        engine.init(&bundle.to_string()).unwrap();
        engine.last_project = Some(project);
        engine.last_compiled = vec![
            (
                PathBuf::from("/__predefined__/StructureDefinition-local-profile.json"),
                differential,
            ),
            (
                PathBuf::from("/__ig__/ImplementationGuide-snapshot-test.json"),
                primary,
            ),
        ];
        let guide = engine
            .site_model_from_compile(
                &PrepareGuideOptions {
                    build_epoch_secs: 1,
                    liquid_asset_dirs: Vec::new(),
                    branch: None,
                    revision: None,
                },
                "test",
                site_build::Sha256Digest::of_bytes(b"snapshot-predefined"),
                site_build::Sha256Digest::of_bytes(b"snapshot-predefined-local"),
            )
            .unwrap();
        let matching = guide
            .resources
            .iter()
            .filter(|resource| resource.key.id == "local-profile")
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert!(matching[0]
            .resource
            .pointer("/snapshot/element")
            .and_then(Value::as_array)
            .is_some_and(|elements| !elements.is_empty()));
    }

    #[test]
    fn prepared_guide_cache_is_an_exact_recipe_bound_gate() {
        let config = "id: demo.ig\nfhirVersion: 4.0.1\n";
        let project = CompiledProjectRevision {
            config: config.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::new(),
            resolved_packages: Some(ResolvedPackages {
                config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
                resolution_support: BTreeSet::new(),
                labels: Vec::new(),
            }),
        };
        let ig = serde_json::json!({
            "resourceType": "ImplementationGuide",
            "id": "demo",
            "packageId": "demo.ig",
            "fhirVersion": ["4.0.1"]
        });
        let mut engine = Engine {
            last_project: Some(project.clone()),
            last_compiled: vec![(PathBuf::from("/__ig__/ImplementationGuide-demo.json"), ig)],
            ..Default::default()
        };
        let input = PrepareGuideOptions {
            build_epoch_secs: 1,
            liquid_asset_dirs: Vec::new(),
            branch: None,
            revision: None,
        };
        let revision = engine
            .site_build_project_revision(&project, "demo.ig")
            .unwrap();
        let lock = engine.site_build_package_lock(&project).unwrap();
        let resolved = project.resolved_packages.as_ref().unwrap();
        let key = engine
            .preparation_cache_keys(&input, &revision, &lock, resolved, "test")
            .unwrap();
        let key = key.prepared_guide;
        let prepared = minimal_prepared_guide();
        engine.prepared_guide_cache = Some(PreparedGuideCacheEntry {
            key: key.clone(),
            guide: prepared.clone(),
        });

        let actual = engine
            .site_model_from_compile(
                &input,
                "test",
                key.clone(),
                site_build::Sha256Digest::of_bytes(b"snapshot-local"),
            )
            .unwrap();
        assert_eq!(actual, prepared);
        assert_eq!(engine.derived_cache_hits.prepared_guide, 1);

        let mut changed = input.clone();
        changed.build_epoch_secs = 2;
        let changed_key = engine
            .preparation_cache_keys(&changed, &revision, &lock, resolved, "test")
            .unwrap()
            .prepared_guide;
        assert_ne!(changed_key, key);
        assert!(engine
            .site_model_from_compile(
                &changed,
                "test",
                changed_key,
                site_build::Sha256Digest::of_bytes(b"changed-snapshot-local"),
            )
            .unwrap_err()
            .contains("resolved closure has no required core package"));
        assert_eq!(engine.derived_cache_hits.prepared_guide, 1);
    }

    #[test]
    fn snapshot_completed_local_cache_key_has_the_exact_invalidation_boundary() {
        let mut engine = Engine {
            last_compiled: vec![(
                PathBuf::from("/__ig__/ImplementationGuide-demo.json"),
                serde_json::json!({
                    "resourceType":"ImplementationGuide",
                    "id":"demo",
                    "packageId":"demo.ig",
                    "fhirVersion":["4.0.1"]
                }),
            )],
            ..Default::default()
        };
        let coordinate = site_build::PackageCoordinate::parse("hl7.fhir.r4.core#4.0.1").unwrap();
        let package = |bytes: &[u8]| {
            site_build::PackageLock::from_packages([site_build::LockedPackage {
                coordinate: coordinate.clone(),
                content: site_build::ContentRef::of_bytes(bytes, None::<String>),
                dependencies: BTreeSet::new(),
            }])
            .unwrap()
        };
        let resolved = ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(b"config"),
            resolution_support: BTreeSet::new(),
            labels: vec!["hl7.fhir.r4.core#4.0.1".into(), "support#1".into()],
        };
        let input = PrepareGuideOptions {
            build_epoch_secs: 1,
            liquid_asset_dirs: vec!["input/includes".into()],
            branch: None,
            revision: None,
        };
        let project = site_build::ProjectRevision {
            project_id: "demo.ig".into(),
            revision: "fixture".into(),
            sources: site_build::SourceManifest::default(),
        };
        let snapshot_key =
            |engine: &Engine, lock: &site_build::PackageLock, resolved: &ResolvedPackages| {
                engine
                    .preparation_cache_keys(&input, &project, lock, resolved, "test")
                    .unwrap()
                    .snapshot_completed_local
            };
        let baseline = snapshot_key(&engine, &package(b"package-a"), &resolved);

        // Authored prose/build metadata are deliberately absent from this key;
        // the complete PreparedGuide key binds them and augmentation reruns.
        assert_eq!(
            baseline,
            snapshot_key(&engine, &package(b"package-a"), &resolved)
        );

        engine.last_compiled[0].1["version"] = Value::String("2".into());
        let compiled_change = snapshot_key(&engine, &package(b"package-a"), &resolved);
        assert_ne!(baseline, compiled_change);
        engine.last_compiled[0]
            .1
            .as_object_mut()
            .unwrap()
            .remove("version");

        let package_change = snapshot_key(&engine, &package(b"package-b"), &resolved);
        assert_ne!(baseline, package_change);

        let mut changed_fixpoint = resolved.clone();
        changed_fixpoint.config_sha256 = site_build::Sha256Digest::of_bytes(b"other-config");
        assert_ne!(
            baseline,
            snapshot_key(&engine, &package(b"package-a"), &changed_fixpoint)
        );
        let mut changed_support = resolved.clone();
        changed_support
            .resolution_support
            .insert("excluded#1".into());
        assert_ne!(
            baseline,
            snapshot_key(&engine, &package(b"package-a"), &changed_support)
        );

        let mut reordered = resolved.clone();
        reordered.labels.reverse();
        let order_change = snapshot_key(&engine, &package(b"package-a"), &reordered);
        assert_ne!(baseline, order_change);
    }

    #[test]
    fn closed_build_cache_key_binds_target_and_diagnostics() {
        let prepared = site_build::Sha256Digest::of_bytes(b"prepared");
        let target = site_build::RenderTarget {
            renderer: site_build::ProducerRef::new("cycle-site", "2"),
            mode: site_build::RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::from([("contract".into(), "cycle-site/v2".into())]),
        };
        let diagnostics = BTreeSet::new();
        let baseline = closed_site_build_cache_key(&prepared, &target, &diagnostics).unwrap();
        let mut changed_target = target.clone();
        changed_target
            .parameters
            .insert("buildEpochSecs".into(), "2".into());
        assert_ne!(
            baseline,
            closed_site_build_cache_key(&prepared, &changed_target, &diagnostics).unwrap()
        );
        let changed_diagnostics = BTreeSet::from([site_build::BuildDiagnostic::new(
            site_build::DiagnosticSeverity::Warning,
            "fixture",
            "warning",
        )]);
        assert_ne!(
            baseline,
            closed_site_build_cache_key(&prepared, &target, &changed_diagnostics).unwrap()
        );
    }
}
