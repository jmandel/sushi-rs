//! `wasm_api` — the wasm-bindgen JS surface for the FSH editor (wasm-editor-plan
//! P2). It keeps `wasm-bindgen` OUT of the core crates: the compiler and the
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
//! const s = new Session();               // or Session.global() for the shared one
//! s.mount(bundlesJson);                  // -> { ok, apiVersion, result: { mounted } }
//! s.prepareAndMount(bundlesJson);        // cold normalize/mount + artifact metadata
//! const artifact = s.takePrepared(label); // direct Uint8Array, intentionally not JSON
//! s.beginPreparedMount(count);           // warm all-or-nothing compact transaction
//! s.stagePreparedMount(bytes, key);      // one checked artifact at a time
//! s.commitPreparedMount();               // publish only after all stages validate
//! s.compile(filesJson, config, predefinedJson);
//! s.snapshot(urlOrInlineSd);
//! s.buildSiteDb(inputJson);
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
//! # Legacy global compatibility
//!
//! The original flat free-function exports are gone: [`Session`] is the only JS
//! API. A regular constructed session owns isolated mutable state. The explicit
//! [`Session::global`] constructor exists only for native compatibility tests and
//! old embedders that deliberately need the former thread-local singleton.
//!
//! Everything runs synchronously in the Worker; the walk engine is the same code
//! the native gates exercise (proven byte-identical by `scripts/wasm-parity.sh`).
//!
//! ## Native build
//! The crate also builds on native targets (JS glue is inert there) so
//! `cargo test --workspace` links it. The real entry points are only meaningful
//! under `wasm32-unknown-unknown` + wasm-bindgen.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

use package_store::{BundleSource, PackageSource};
use render_page::{
    all_compile_inputs, collect_stock_revision, ArtifactObservation, PageArtifactReadSet,
    StockFragmentPolicy, StockInput, StockPage, StockPageOutcome, STOCK_PAGE_SOURCE_NAMESPACE,
    STOCK_RUNTIME_INPUT_NAMESPACE, STOCK_SITE_DATA_NAMESPACE, STOCK_STAGED_INCLUDE_NAMESPACE,
    STOCK_TEMPLATE_INCLUDE_NAMESPACE,
};

mod render_surface;
#[cfg(test)]
use render_surface::build_render_state;
use render_surface::{
    build_render_semantics, build_render_state_from_semantics, RenderSemantics, RenderState,
    SiteOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::prelude::*;

/// The result/error envelope + apiVersion are the SHARED implementation
/// (`api_envelope`) — one schema for the Session and the `fig` CLI's `--json`.
use api_envelope::{envelope, envelope_ser, API_VERSION};

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
// `Session` owns one Engine. Only `Session::global()` uses the explicit legacy
// thread-local compatibility instance below.
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
    /// Last `compile()` outputs `(synthetic path, body)`, indexed as local
    /// resources for snapshot base resolution.
    last_compiled: Vec<(PathBuf, Value)>,
    /// Authored inputs that can affect the semantic render set. Site-file
    /// CONTENT is deliberately absent: only its page listing reaches IG export.
    /// This lets prose-only edits retain an exactly-equal FragmentEngine while
    /// config, FSH, predefined resources, or the narrative tree invalidate it.
    last_render_semantic_inputs: Option<RenderSemanticInputs>,
    /// Exact authored inputs for the last successful `compileProject` call.
    /// Legacy `compile` deliberately leaves this absent, preventing it from
    /// masquerading as a complete project revision.
    last_project: Option<CompiledProjectRevision>,
    last_compile_diagnostics: Vec<DiagnosticJs>,
    /// The mounted site tree (template statics + staged pagecontent + _data +
    /// _includes + optional txcache), keyed by virtual path under /site.
    site_files: std::collections::HashMap<PathBuf, Vec<u8>>,
    site_options: SiteOptions,
    /// `menu.xml` generated from the last `compile()`'s sushi-config `menu:` tree
    /// (the navbar is IG data, not template chrome). `produceStockSite` stages it
    /// into `_includes/` so the layouts' `{% include menu.xml %}` resolves.
    menu_xml: Option<String>,
    /// Input page-folder listing (`input/{pagecontent,pages,resource-docs}` ->
    /// filenames) threaded into IG export so the generated IG's `definition.page`
    /// narrative tree is complete (narrative titles + the artifacts-section number).
    /// Set via `set_page_listing`; empty by default (artifact layer needs no pages).
    page_listing: std::collections::HashMap<String, Vec<String>>,
    /// Expensive semantic renderer, shared by any number of site-only
    /// generations. Its internal tree contains only snapshot-complete `/own`,
    /// packages, and txcache, so page/template overlays cannot make it stale.
    render_semantics: Option<Rc<RenderSemantics>>,
    /// Cheap page/template surface for the current mounted site generation.
    render_state: Option<Rc<RenderState>>,
    /// Frozen native-template executions keyed by their content-derived
    /// SiteBuild id. Unlike `render_state`, these survive later mounts/compiles:
    /// a handle always renders the exact tree and semantic context it opened.
    stock_builds: BTreeMap<String, StockBuildRuntime>,
    /// Renderer-neutral semantics derived from the current (or an earlier)
    /// exact compile revision. The entry is reusable only when its canonical
    /// key matches every authored byte, locked package, compiled value,
    /// preparation option, and preparation recipe below.
    prepared_guide_cache: Option<PreparedGuideCacheEntry>,
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
    /// Populated only when a compatibility caller explicitly asks for rows.
    /// Cycle v2 never constructs this value.
    site_db: Option<site_db::SiteDb>,
}

#[derive(Clone)]
struct ClosedSiteBuildCacheEntry {
    key: site_build::Sha256Digest,
    result: SiteBuildFromCompileResult,
}

#[cfg(test)]
#[derive(Default)]
struct DerivedCacheHits {
    prepared_guide: u64,
    closed_site_build: u64,
}

#[derive(Clone)]
struct StockBuildRuntime {
    state: Rc<RenderState>,
    build: site_build::SiteBuild,
    pages: BTreeMap<String, StockRenderedPage>,
    provenance_attributes: BTreeMap<String, String>,
}

#[derive(Clone)]
struct StockRenderedPage {
    html: String,
    reads: PageArtifactReadSet,
}

#[derive(Clone, Debug)]
struct MountedPackage {
    content: site_build::ContentRef,
    declared_dependencies: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct ResolvedPackages {
    config_sha256: site_build::Sha256Digest,
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

#[derive(Clone, Debug, PartialEq)]
struct RenderSemanticInputs {
    config: String,
    fsh: BTreeMap<String, String>,
    predefined: BTreeMap<String, Value>,
    page_listing: BTreeMap<String, Vec<String>>,
}

thread_local! {
    /// Compatibility engine used only by the explicitly requested
    /// `Session::global()` handle. Regular `new Session()` instances own isolated
    /// state; sharing package bytes belongs at the immutable package-store layer,
    /// not in mutable compile/template/site fields.
    static ENGINE: RefCell<Engine> = RefCell::new(Engine::default());
}

fn set_panic_hook() {
    #[cfg(target_family = "wasm")]
    console_error_panic_hook::set_once();
}

impl Engine {
    fn invalidate_render_surface(&mut self) {
        self.render_state = None;
    }

    fn invalidate_render_semantics(&mut self) {
        self.render_semantics = None;
        self.invalidate_render_surface();
    }

    /// Commit a successful compile generation. Exact render-set equality alone
    /// is insufficient: raw config/FSH/predefined/page-listing inputs are also
    /// part of the key so no relevant compiler context is hidden. Packages,
    /// txcache, active-tables, and run UUID are invalidated at their own mutation
    /// boundaries. Ordinary site bytes and template chrome are intentionally not
    /// FragmentEngine inputs.
    fn replace_compiled_render_set(
        &mut self,
        compiled: Vec<(PathBuf, Value)>,
        inputs: RenderSemanticInputs,
    ) {
        let same_semantics = self.last_compiled == compiled
            && self.last_render_semantic_inputs.as_ref() == Some(&inputs);
        self.last_compiled = compiled;
        self.last_render_semantic_inputs = Some(inputs);
        if same_semantics {
            self.invalidate_render_surface();
        } else {
            self.prepared_guide_cache = None;
            self.closed_site_build_cache = None;
            self.invalidate_render_semantics();
        }
    }

    fn replace_local_render_set(&mut self, compiled: Vec<(PathBuf, Value)>) {
        self.last_compiled = compiled;
        self.last_render_semantic_inputs = None;
        self.prepared_guide_cache = None;
        self.closed_site_build_cache = None;
        self.invalidate_render_semantics();
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
        self.last_render_semantic_inputs = None;
        self.last_project = None;
        self.last_compile_diagnostics.clear();
        self.prepared_guide_cache = None;
        self.closed_site_build_cache = None;
        self.invalidate_render_semantics();
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
            self.invalidate_render_semantics();
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

    /// Compatibility entry point for validating artifacts carried in one
    /// contiguous binary batch before appending any immutable package layer.
    /// The browser warm path uses staged per-artifact transactions instead, so
    /// it never constructs a closure-sized JavaScript batch.
    fn mount_prepared_batch(
        &mut self,
        bytes: Vec<u8>,
        manifest_json: &str,
    ) -> Result<PackageMountResult, String> {
        if self.bundle.is_none() {
            return Err("mountPreparedBatch: engine not initialized; call init() first".into());
        }
        let started = clock_ms();
        let compression_before = self
            .bundle
            .as_ref()
            .map(|source| source.compression_metrics())
            .unwrap_or_default();
        let entries: Vec<PreparedBatchEntry> = serde_json::from_str(manifest_json)
            .map_err(|error| format!("mountPreparedBatch: bad manifest JSON: {error}"))?;
        let parsed_at = clock_ms();
        if entries.is_empty() {
            return Err("mountPreparedBatch: manifest has no packages".into());
        }
        let requested = entries.len() as u32;
        let artifact_bytes = bytes.len() as u64;
        let backing = package_store::PreparedArtifactBacking::new(bytes);
        let mut cursor = 0usize;
        let mut prepared = Vec::with_capacity(entries.len());
        for entry in entries {
            let offset: usize = entry
                .offset
                .try_into()
                .map_err(|_| "mountPreparedBatch: offset exceeds host size")?;
            let length: usize = entry
                .byte_length
                .try_into()
                .map_err(|_| "mountPreparedBatch: byteLength exceeds host size")?;
            if offset != cursor {
                return Err(format!(
                    "mountPreparedBatch: artifacts must be contiguous and ordered; expected offset {cursor}, got {offset}"
                ));
            }
            let end = offset
                .checked_add(length)
                .ok_or("mountPreparedBatch: artifact range overflow")?;
            if end > backing.len() {
                return Err("mountPreparedBatch: artifact range exceeds binary batch".into());
            }
            let key: package_store::PreparedPackageKey = entry
                .cache_key
                .parse()
                .map_err(|error| format!("mountPreparedBatch: invalid cache key: {error:#}"))?;
            prepared.push(
                package_store::PreparedPackage::decode_backing_range(
                    backing.clone(),
                    offset..end,
                    &key,
                )
                .map_err(|error| format!("mountPreparedBatch: invalid artifact: {error:#}"))?,
            );
            cursor = end;
        }
        if cursor != backing.len() {
            return Err("mountPreparedBatch: binary batch has unreferenced trailing bytes".into());
        }
        let indexed_members = prepared
            .iter()
            .map(|package| package.files.len() as u64)
            .sum();
        let decoded = clock_ms();
        let mounted = self.commit_prepared(prepared, "mountPreparedBatch")?;
        let compression = compression_delta(
            compression_before,
            self.bundle
                .as_ref()
                .map(|source| source.compression_metrics())
                .unwrap_or_default(),
        );
        let finished = clock_ms();
        Ok(PackageMountResult {
            mounted: self.packages.len() as u32,
            added: mounted,
            packages: requested,
            manifest_json_bytes: manifest_json.len() as u64,
            artifact_bytes,
            retained_blob_bytes: artifact_bytes,
            indexed_members,
            member_body_copies: 0,
            manifest_parse_ms: (parsed_at - started).max(0.0),
            decode_validate_ms: (decoded - parsed_at).max(0.0),
            mount_ms: (finished - decoded).max(0.0),
            compression,
        })
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
            self.invalidate_render_semantics();
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
    /// Legacy `compile`/`setLocalResources` revisions have no such certificate
    /// and retain the historical all-mounted behavior.
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

    /// Compile a project in memory. Returns the [`CompileResult`] payload and
    /// stashes the compiled resources as snapshot-resolution locals.
    fn compile(
        &mut self,
        files_json: &str,
        config: &str,
        predefined_json: &str,
    ) -> Result<CompileResult, String> {
        self.compile_with_resolved(files_json, config, predefined_json, None)
    }

    fn compile_with_resolved(
        &mut self,
        files_json: &str,
        config: &str,
        predefined_json: &str,
        resolved: Option<&ResolvedPackages>,
    ) -> Result<CompileResult, String> {
        let (source, cache_root, _packages) = match resolved {
            Some(resolved) => self.source_for_resolved(resolved)?,
            None => self.source()?,
        };

        // FSH files: object -> Vec sorted by path (matches the disk walk order).
        let files_map: std::collections::BTreeMap<String, String> =
            serde_json::from_str(files_json)
                .map_err(|e| format!("compile: bad files JSON: {e}"))?;
        let fsh_files: Vec<(String, String)> = files_map
            .iter()
            .map(|(path, source)| (path.clone(), source.clone()))
            .collect();

        // Predefined resources: object path -> body. Sorted by path so
        // `PredefinedPackage::load_from` sees the disk-equivalent order.
        let predefined_map: BTreeMap<String, Value> = if predefined_json.trim().is_empty() {
            BTreeMap::new()
        } else {
            serde_json::from_str(predefined_json)
                .map_err(|e| format!("compile: bad predefined JSON: {e}"))?
        };
        let predefined: Vec<(PathBuf, Value)> = predefined_map
            .iter()
            .map(|(path, body)| (PathBuf::from(path), body.clone()))
            .collect();
        let semantic_inputs = RenderSemanticInputs {
            config: config.to_string(),
            fsh: files_map,
            predefined: predefined_map,
            page_listing: self
                .page_listing
                .iter()
                .map(|(directory, names)| {
                    let mut names = names.clone();
                    names.sort();
                    (directory.clone(), names)
                })
                .collect(),
        };

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
        // example markers — instead of the minimal produceStockSite synthesis. The
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
            let fname = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("resource.json");
            render_set.push((
                PathBuf::from(format!("/__predefined__/{fname}")),
                body.clone(),
            ));
        }
        // The generated ImplementationGuide joins the render set so produceStockSite
        // finds a faithful IG (correct titles/example markers/page tree) rather than
        // falling back to the minimal synthesis.
        if let Some(ig) = ig_resource {
            render_set.push((PathBuf::from(format!("/__ig__/{}", ig.filename)), ig.body));
        }
        self.replace_compiled_render_set(render_set, semantic_inputs);

        // Generate the navbar (menu.xml) from the sushi-config `menu:` tree so
        // produceStockSite can stage it into _includes/ — SUSHI writes this per-IG;
        // it is IG data, not template chrome (the template only supplies the
        // `{% include menu.xml %}` point).
        self.menu_xml = compiler::menu::menu_xml(config);

        let resources: Vec<CompiledResourceJs> = compiled
            .into_iter()
            .map(|r| {
                let rt = r
                    .body
                    .get("resourceType")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let id = r
                    .body
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let url = r
                    .body
                    .get("url")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                CompiledResourceJs {
                    filename: r.filename,
                    text: r.text,
                    resource_type: rt,
                    id,
                    url,
                }
            })
            .collect();

        let diagnostics: Vec<DiagnosticJs> = diagnostics
            .into_iter()
            .map(|d| DiagnosticJs {
                severity: d.severity.to_string(),
                message: d.message,
                file: d.file,
                line: d.line,
            })
            .collect();

        // `compile()` itself does not prove that authored site bytes were in
        // scope. `compileProject()` immediately replaces this with its complete
        // input capture after this successful semantic compile.
        self.last_project = None;
        self.last_compile_diagnostics = diagnostics.clone();

        Ok(CompileResult {
            resources,
            diagnostics,
            timings: Timings::default(),
        })
    }

    /// Compile with the authored site-file manifest in scope. This is the normal
    /// editor build entry point: page-folder names must reach IG export during the
    /// ONE compile, so a later site.db projection can consume `last_compiled`
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
        self.page_listing = page_listing_from_site_files(&site_files);
        let result = self.compile_with_resolved(
            files_json,
            config,
            predefined_json,
            Some(&resolved_packages),
        )?;
        self.last_project = Some(CompiledProjectRevision {
            config: config.to_string(),
            fsh,
            predefined,
            site_files,
            resolved_packages: Some(resolved_packages),
        });
        Ok(result)
    }

    /// Set the "local" StructureDefinitions the next snapshot resolves bases
    /// against — the in-memory equivalent of the CLI's `--local-dir`. Replaces the
    /// local set from the last `compile()`. Returns the count.
    fn set_local_resources(&mut self, json: &str) -> Result<u32, String> {
        let map: std::collections::BTreeMap<String, Value> = serde_json::from_str(json)
            .map_err(|e| format!("set_local_resources: bad JSON: {e}"))?;
        let locals: Vec<(PathBuf, Value)> = map
            .into_iter()
            .map(|(p, v)| (PathBuf::from(format!("/__local__/{p}")), v))
            .collect();
        let n = locals.len() as u32;
        self.replace_local_render_set(locals);
        self.last_project = None;
        self.last_compile_diagnostics.clear();
        Ok(n)
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

    /// Build the site.db ROW MODEL from fully in-memory IG inputs. Returns the row
    /// model as a `Value` (SQLite/`core/db.ts` column casing).
    fn build_site_db(&self, input_json: &str) -> Result<Value, String> {
        let input: SiteDbInput = serde_json::from_str(input_json)
            .map_err(|e| format!("build_site_db: bad input JSON: {e}"))?;
        validate_predefined_site_overlap(&input.predefined, &input.site_files, "build_site_db")?;

        let (candidate_source, candidate_root, _packages) = self.source()?;
        let index = package_store::version_index_from_cache(&candidate_source, &candidate_root);
        let step = package_store::resolve_project(
            &input.config,
            &candidate_source,
            &candidate_root,
            Some(&index),
        )
        .map_err(|error| format!("build_site_db: resolve package closure: {error:#}"))?;
        if !step.satisfied {
            return Err(format!(
                "build_site_db: mounted packages do not satisfy the project closure: {}",
                serde_json::to_string(&step.missing).unwrap_or_else(|_| "[]".into())
            ));
        }
        let resolved = resolved_packages_from_step(&input.config, &step)?;
        let (source, cache_root, package_context) = self.source_for_resolved(&resolved)?;
        let cache = cache_root.to_string_lossy().into_owned();

        // ---- S1/S2 (+ IG export): compile in memory, producing the IG resource. ----
        let fsh_files: Vec<(String, String)> = input.fsh.into_iter().collect();
        let predefined: Vec<(PathBuf, Value)> = input
            .predefined
            .into_iter()
            .map(|(p, v)| (PathBuf::from(p), v))
            .collect();
        // The page-folder listing ig_export needs (folder -> filenames) is derived
        // from the site_files map: the disk path would scan input/{pagecontent,
        // pages,resource-docs}; we hand it the same names from the VFS.
        let page_dir_listing = page_listing_from_site_files(&input.site_files);

        let (conformance, ig_resource, _diagnostics) = compiler::build_project_in_memory_with_ig(
            &input.config,
            &fsh_files,
            predefined,
            source.clone(),
            &cache,
            page_dir_listing,
        )
        .map_err(|e| format!("build_site_db: compile failed: {e:#}"))?;

        // ---- S3: snapshot-complete each StructureDefinition against the same
        // exact package fixpoint used by the compile. Core remains a validated
        // distinguished member; external dependency bases remain resolvable.
        let fhir_version = ig_resource
            .as_ref()
            .and_then(|resource| implementation_guide_fhir_version(&resource.body))
            .ok_or("build_site_db: compiled ImplementationGuide has no fhirVersion")?;
        self.resolved_core_package(&resolved, &input.config, &fhir_version, "build_site_db")?;
        let mut ctx = snapshot_gen::PackageContext::new_with(source, &cache_root, &package_context)
            .map_err(|e| format!("build_site_db: package context: {e:#}"))?;
        let locals: Vec<(PathBuf, Value)> = conformance
            .iter()
            .map(|r| {
                (
                    PathBuf::from(format!("/__compiled__/{}", r.filename)),
                    r.body.clone(),
                )
            })
            .collect();
        ctx.load_local_resources(locals);

        let mut generated: Vec<Value> = Vec::new();
        for r in &conformance {
            if r.body.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                let snap =
                    snapshot_gen::generate_snapshot(r.body.clone(), &ctx, Default::default())
                        .map_err(|e| format!("build_site_db: snapshot {}: {e:#}", r.filename))?;
                generated.push(snap);
            } else {
                generated.push(r.body.clone());
            }
        }
        let primary_implementation_guide = ig_resource.as_ref().map(|ig| ig.body.clone()).ok_or(
            "build_site_db: no ImplementationGuide produced (FSHOnly config or missing id)",
        )?;
        generated.push(primary_implementation_guide.clone());

        // Predefined `input/resources/**` bodies are the examples (S5 loadResources).
        let examples: Vec<Value> = collect_example_resources(&input.site_files, "build_site_db")?;

        assemble_site_db_value(
            "build_site_db",
            &generated,
            &primary_implementation_guide,
            &examples,
            &input.config,
            &input.site_files,
            input.build_epoch_secs,
            &input.liquid_asset_dirs,
            input.branch,
            input.revision,
        )
    }

    /// Project the exact most recent `compileProject` revision into site.db rows.
    /// Unlike the legacy `build_site_db`, this path has no FSH inputs and therefore
    /// cannot accidentally perform a second semantic compile.
    fn build_site_db_from_compile(&mut self, input_json: &str) -> Result<Value, String> {
        let input: SiteDbFromCompileInput = serde_json::from_str(input_json)
            .map_err(|e| format!("build_site_db_from_compile: bad input JSON: {e}"))?;
        let project = self
            .validate_project_projection(&input, "build_site_db_from_compile")?
            .clone();
        let (project_id, _) = compiled_ig_identity(&self.last_compiled)?;
        let project_revision = self.site_build_project_revision(&project, &project_id)?;
        let package_lock = self.site_build_package_lock(&project)?;
        let prepared_key = self.prepared_guide_cache_key(
            &input,
            &project_revision,
            &package_lock,
            "build_site_db_from_compile",
        )?;
        let (_, db) =
            self.site_model_from_compile(&input, "build_site_db_from_compile", true, prepared_key)?;
        serde_json::to_value(db.expect("row projection requested"))
            .map_err(|e| format!("build_site_db_from_compile: serialize rows: {e}"))
    }

    /// Build a callback-free Cycle handoff from the exact preceding compile.
    /// `cycle-site/v2` returns four typed semantic objects plus raw asset objects;
    /// omitted `target` retains the aggregate v1 compatibility contract. Both
    /// use the same digest-indexed CAS transport and neither can invoke a second
    /// compile or a request-time fragment callback.
    fn build_site_build_from_compile(
        &mut self,
        input_json: &str,
    ) -> Result<SiteBuildFromCompileResult, String> {
        let input: SiteDbFromCompileInput = serde_json::from_str(input_json)
            .map_err(|e| format!("build_site_build_from_compile: bad input JSON: {e}"))?;
        let project = self
            .validate_project_projection(&input, "build_site_build_from_compile")?
            .clone();
        let target = input.target.as_deref().unwrap_or("cycle-site/v1");
        if !matches!(target, "cycle-site/v1" | "cycle-site/v2") {
            return Err(format!(
                "build_site_build_from_compile: unsupported target {target:?}"
            ));
        }
        let (project_id, fhir_version) = compiled_ig_identity(&self.last_compiled)?;
        let project_revision = self.site_build_project_revision(&project, &project_id)?;
        let package_lock = self.site_build_package_lock(&project)?;
        let prepared_key = self.prepared_guide_cache_key(
            &input,
            &project_revision,
            &package_lock,
            "build_site_build_from_compile",
        )?;

        let diagnostics = site_build_diagnostics(&self.last_compile_diagnostics);

        let mut parameters = BTreeMap::from([
            ("contract".into(), target.into()),
            ("buildEpochSecs".into(), input.build_epoch_secs.to_string()),
            (
                "liquidAssetDirs".into(),
                if input.liquid_asset_dirs.is_empty() {
                    "input/includes".into()
                } else {
                    input.liquid_asset_dirs.join(",")
                },
            ),
        ]);
        if let Some(branch) = &input.branch {
            parameters.insert("branch".into(), branch.clone());
        }
        if let Some(revision) = &input.revision {
            parameters.insert("revision".into(), revision.clone());
        }

        let render_target = site_build::RenderTarget {
            renderer: site_build::ProducerRef::new(
                "cycle-site",
                if target == "cycle-site/v2" { "2" } else { "1" },
            ),
            mode: site_build::RenderMode::ExternalBuilder,
            fhir_version,
            template: None,
            parameters,
        };

        let closed_key =
            closed_site_build_cache_key(&prepared_key, &render_target, &diagnostics, target)
                .map_err(|error| format!("build_site_build_from_compile: cache key: {error}"))?;
        if let Some(entry) = &self.closed_site_build_cache {
            if entry.key == closed_key {
                #[cfg(test)]
                {
                    self.derived_cache_hits.closed_site_build += 1;
                }
                return Ok(entry.result.clone());
            }
        }

        let (prepared_guide, db) = self.site_model_from_compile(
            &input,
            "build_site_build_from_compile",
            target == "cycle-site/v1",
            prepared_key,
        )?;

        let result = if target == "cycle-site/v2" {
            let input = site_build::cycle_semantic::CycleProjectionInput {
                project: project_revision,
                package_lock,
                render_target,
                diagnostics,
            };
            let projection = site_build::cycle_semantic::close_prepared(&prepared_guide, input)
                .map_err(|e| format!("build_site_build_from_compile: {e}"))?;
            SiteBuildFromCompileResult {
                transport_version: SITE_BUILD_CAS_TRANSPORT.into(),
                site_build: projection.site_build,
                objects: encode_cas_objects(projection.objects),
                site_db_json: None,
            }
        } else {
            let projection = site_build::site_db_compat::close_projection(
                db.as_ref().expect("v1 requests SiteDb projection"),
                site_build::site_db_compat::CloseProjectionInput {
                    project: project_revision,
                    package_lock,
                    render_target,
                    diagnostics,
                },
            )
            .map_err(|e| format!("build_site_build_from_compile: {e}"))?;
            let site_db_json = String::from_utf8(projection.bytes.clone())
                .map_err(|e| format!("build_site_build_from_compile: site.db UTF-8: {e}"))?;
            let content =
                site_build::ContentRef::of_bytes(&projection.bytes, Some("application/json"));
            SiteBuildFromCompileResult {
                transport_version: SITE_BUILD_CAS_TRANSPORT.into(),
                site_build: projection.site_build,
                objects: BTreeMap::from([(
                    content.sha256.to_string(),
                    base64_encode(&projection.bytes),
                )]),
                site_db_json: Some(site_db_json),
            }
        };
        self.closed_site_build_cache = Some(ClosedSiteBuildCacheEntry {
            key: closed_key,
            result: result.clone(),
        });
        Ok(result)
    }

    fn validate_project_projection<'a>(
        &'a self,
        input: &SiteDbFromCompileInput,
        operation: &str,
    ) -> Result<&'a CompiledProjectRevision, String> {
        let project = self.last_project.as_ref().ok_or_else(|| {
            format!(
                "{operation}: compileProject has not established a complete source revision; call compileProject first"
            )
        })?;
        if project.config != input.config {
            return Err(format!(
                "{operation}: projection config differs from the compiled revision"
            ));
        }
        if project.fsh != input.fsh {
            return Err(format!(
                "{operation}: projection FSH files differ from the compiled revision"
            ));
        }
        if project.predefined != input.predefined {
            return Err(format!(
                "{operation}: projection predefined resources differ from the compiled revision"
            ));
        }
        if project.site_files != input.site_files {
            return Err(format!(
                "{operation}: projection site files differ from the compiled revision"
            ));
        }
        Ok(project)
    }

    /// Canonical lookup identity for the expensive snapshot + semantic
    /// preparation phase. This is intentionally independent of Cycle v1/v2:
    /// both projections consume the same PreparedGuide. The key includes the
    /// actual compiled values as a defensive binding in addition to exact
    /// source/package identities, so a caller can never turn a hidden compiler
    /// state difference into a false cache hit.
    fn prepared_guide_cache_key(
        &self,
        input: &SiteDbFromCompileInput,
        project: &site_build::ProjectRevision,
        package_lock: &site_build::PackageLock,
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
        let compiled_sha256 = site_build::sha256_canonical(&compiled)
            .map_err(|error| format!("{operation}: hash compiled revision: {error}"))?;
        let payload = PreparedGuideCachePayload {
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
        };
        site_build::sha256_canonical(&payload)
            .map_err(|error| format!("{operation}: hash PreparedGuide cache key: {error}"))
    }

    fn site_model_from_compile(
        &mut self,
        input: &SiteDbFromCompileInput,
        operation: &str,
        with_rows: bool,
        cache_key: site_build::Sha256Digest,
    ) -> Result<(site_build::PreparedGuide, Option<site_db::SiteDb>), String> {
        if self.last_compiled.is_empty() {
            return Err(format!(
                "{operation}: no compiled revision; call compileProject first"
            ));
        }

        self.validate_project_projection(input, operation)?;
        if let Some(entry) = &self.prepared_guide_cache {
            if entry.key == cache_key && (!with_rows || entry.site_db.is_some()) {
                #[cfg(test)]
                {
                    self.derived_cache_hits.prepared_guide += 1;
                }
                return Ok((entry.guide.clone(), entry.site_db.clone()));
            }
        }

        let project = self.validate_project_projection(input, operation)?;
        let (_, fhir_version) = compiled_ig_identity(&self.last_compiled)?;
        let resolved = project.resolved_packages.as_ref().ok_or_else(|| {
            format!("{operation}: compiled revision has no bound package resolver fixpoint")
        })?;
        // Validate the distinguished core, but snapshot against the complete
        // exact closure used by compileProject so profiles may derive from
        // external dependencies without consulting unrelated mounted versions.
        self.resolved_core_package(resolved, &input.config, &fhir_version, operation)?;
        let (source, cache_root, package_context) = self.source_for_resolved(resolved)?;
        let mut ctx = snapshot_gen::PackageContext::new_with(source, &cache_root, &package_context)
            .map_err(|e| format!("{operation}: package context: {e:#}"))?;
        let compiled_locals: Vec<(PathBuf, Value)> = self
            .last_compiled
            .iter()
            .filter(|(path, _)| path.starts_with("/__compiled__"))
            .cloned()
            .collect();
        ctx.load_local_resources(compiled_locals);

        let mut generated = Vec::new();
        let mut primary_implementation_guide = None;
        for (path, body) in &self.last_compiled {
            if path.starts_with("/__compiled__") {
                if body.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                    let label = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("StructureDefinition");
                    generated.push(
                        snapshot_gen::generate_snapshot(body.clone(), &ctx, Default::default())
                            .map_err(|e| format!("{operation}: snapshot {label}: {e:#}"))?,
                    );
                } else {
                    generated.push(body.clone());
                }
            } else if path.starts_with("/__ig__") {
                generated.push(body.clone());
                if primary_implementation_guide.replace(body.clone()).is_some() {
                    return Err(format!(
                        "{operation}: compiled revision has multiple primary ImplementationGuide artifacts"
                    ));
                }
            }
        }
        let primary_implementation_guide = primary_implementation_guide.ok_or_else(|| {
            format!("{operation}: compiled revision has no primary ImplementationGuide")
        })?;

        let examples = collect_example_resources(&input.site_files, operation)?;
        if with_rows {
            let outcome = assemble_site_db_model(
                operation,
                &generated,
                &primary_implementation_guide,
                &examples,
                &input.config,
                &input.site_files,
                input.build_epoch_secs,
                &input.liquid_asset_dirs,
                input.branch.clone(),
                input.revision.clone(),
            )?;
            let guide = outcome.prepared_guide;
            let db = outcome.db;
            self.prepared_guide_cache = Some(PreparedGuideCacheEntry {
                key: cache_key,
                guide: guide.clone(),
                site_db: Some(db.clone()),
            });
            Ok((guide, Some(db)))
        } else {
            let outcome = assemble_prepared_model(
                operation,
                &generated,
                &primary_implementation_guide,
                &examples,
                &input.config,
                &input.site_files,
                input.build_epoch_secs,
                &input.liquid_asset_dirs,
                input.branch.clone(),
                input.revision.clone(),
            )?;
            let guide = outcome.prepared_guide;
            self.prepared_guide_cache = Some(PreparedGuideCacheEntry {
                key: cache_key,
                guide: guide.clone(),
                site_db: None,
            });
            Ok((guide, None))
        }
    }

    fn site_build_project_revision(
        &self,
        project: &CompiledProjectRevision,
        project_id: &str,
    ) -> Result<site_build::ProjectRevision, String> {
        validate_predefined_site_overlap(
            &project.predefined,
            &project.site_files,
            "build_site_build_from_compile",
        )?;
        let mut entries: BTreeMap<site_build::SourcePath, site_build::SourceEntry> =
            BTreeMap::new();
        let mut insert = |path: &str,
                          kind: site_build::SourceKind,
                          bytes: &[u8],
                          media_type: &str|
         -> Result<(), String> {
            let path = site_build::SourcePath::parse(path.to_string())
                .map_err(|e| format!("build_site_build_from_compile: source path {path}: {e}"))?;
            let entry = site_build::SourceEntry {
                kind,
                content: site_build::ContentRef::of_bytes(bytes, Some(media_type)),
            };
            if entries.insert(path.clone(), entry).is_some() {
                return Err(format!(
                    "build_site_build_from_compile: source path {path} is declared by more than one input channel"
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
            let bytes = site_build::canonical_json_bytes(body).map_err(|e| {
                format!("build_site_build_from_compile: canonical predefined {path}: {e}")
            })?;
            insert(
                path,
                site_build::SourceKind::PredefinedResource,
                &bytes,
                "application/fhir+json",
            )?;
        }
        for (path, encoded) in &project.site_files {
            let bytes = base64_decode(encoded).map_err(|e| {
                format!("build_site_build_from_compile: bad base64 source {path}: {e}")
            })?;
            let (kind, media_type) = source_kind_and_media_type(path);
            insert(path, kind, &bytes, media_type)?;
        }

        let sources = site_build::SourceManifest::from_entries(entries)
            .map_err(|e| format!("build_site_build_from_compile: source manifest: {e}"))?;
        let revision = format!(
            "sources-sha256:{}",
            site_build::sha256_canonical(&sources)
                .map_err(|e| format!("build_site_build_from_compile: source revision: {e}"))?
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
        let resolved = project.resolved_packages.as_ref().ok_or(
            "build_site_build_from_compile: compiled revision has no bound package resolver fixpoint",
        )?;
        if resolved.config_sha256 != site_build::Sha256Digest::of_bytes(project.config.as_bytes()) {
            return Err(
                "build_site_build_from_compile: compiled package closure belongs to different config bytes"
                    .into(),
            );
        }

        let mut coordinates_by_id: BTreeMap<String, Vec<site_build::PackageCoordinate>> =
            BTreeMap::new();
        let mut ordered = Vec::new();
        for label in &resolved.labels {
            let coordinate = site_build::PackageCoordinate::parse(label).map_err(|e| {
                format!("build_site_build_from_compile: non-exact resolved package {label}: {e}")
            })?;
            coordinates_by_id
                .entry(coordinate.package_id().to_string())
                .or_default()
                .push(coordinate.clone());
            ordered.push((label, coordinate));
        }

        let mut packages = Vec::new();
        for (label, coordinate) in ordered {
            let material = self.package_materials.get(label).ok_or_else(|| {
                format!("build_site_build_from_compile: resolved package {label} has no mounted material")
            })?;
            let dependencies = material
                .declared_dependencies
                .iter()
                .filter_map(|(package_id, requested)| {
                    coordinates_by_id
                        .get(package_id)
                        .map(|candidates| (package_id, requested, candidates))
                })
                .map(|(package_id, requested, candidates)| {
                    select_locked_dependency(package_id, requested, candidates)
                })
                .collect::<Result<BTreeSet<_>, _>>()?;
            packages.push(site_build::LockedPackage {
                coordinate,
                content: material.content.clone(),
                dependencies,
            });
        }
        site_build::PackageLock::from_packages(packages)
            .map_err(|e| format!("build_site_build_from_compile: package lock: {e}"))
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
    Ok(ResolvedPackages {
        config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
        labels,
    })
}

#[allow(clippy::too_many_arguments)]
fn assemble_site_db_value(
    operation: &str,
    generated: &[Value],
    primary_implementation_guide: &Value,
    examples: &[Value],
    config: &str,
    site_files: &std::collections::BTreeMap<String, String>,
    build_epoch_secs: i64,
    liquid_asset_dirs: &[String],
    branch: Option<String>,
    revision: Option<String>,
) -> Result<Value, String> {
    let models = assemble_site_db_model(
        operation,
        generated,
        primary_implementation_guide,
        examples,
        config,
        site_files,
        build_epoch_secs,
        liquid_asset_dirs,
        branch,
        revision,
    )?;
    serde_json::to_value(models.db).map_err(|e| format!("{operation}: serialize rows: {e}"))
}

#[allow(clippy::too_many_arguments)]
fn assemble_site_db_model(
    operation: &str,
    generated: &[Value],
    primary_implementation_guide: &Value,
    examples: &[Value],
    config: &str,
    site_files: &BTreeMap<String, String>,
    build_epoch_secs: i64,
    liquid_asset_dirs: &[String],
    branch: Option<String>,
    revision: Option<String>,
) -> Result<site_db::BuildOutcome, String> {
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
    let outcome = site_db::build_from_inputs(&site_db::InMemoryInputs {
        generated,
        primary_implementation_guide,
        examples,
        sushi_config_yaml: config,
        build_epoch_secs,
        branch,
        revision,
        vfs,
        ig_root,
        liquid_asset_rel_dirs,
    })
    .map_err(|e| format!("{operation}: assemble rows: {e:#}"))?;
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
fn assemble_prepared_model(
    operation: &str,
    generated: &[Value],
    primary_implementation_guide: &Value,
    examples: &[Value],
    config: &str,
    site_files: &BTreeMap<String, String>,
    build_epoch_secs: i64,
    liquid_asset_dirs: &[String],
    branch: Option<String>,
    revision: Option<String>,
) -> Result<site_db::PreparedBuildOutcome, String> {
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
    site_db::prepare_from_inputs(&site_db::InMemoryInputs {
        generated,
        primary_implementation_guide,
        examples,
        sushi_config_yaml: config,
        build_epoch_secs,
        branch,
        revision,
        vfs,
        ig_root,
        liquid_asset_rel_dirs,
    })
    .map_err(|e| format!("{operation}: prepare guide: {e:#}"))
}

/// Run `f` against the process-global engine.
fn with_engine<T>(f: impl FnOnce(&mut Engine) -> T) -> T {
    ENGINE.with(|e| f(&mut e.borrow_mut()))
}

// ---------------------------------------------------------------------------
// JS-facing result shapes (serde -> JSON string the Worker JSON.parse()s, the
// simplest robust bindgen contract).
// ---------------------------------------------------------------------------

#[derive(Serialize)]
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

#[derive(Serialize)]
struct CompiledResourceJs {
    filename: String,
    /// The exact bytes SUSHI writes (already FHIR-canonical JSON as a string).
    text: String,
    #[serde(rename = "resourceType")]
    resource_type: Option<String>,
    id: Option<String>,
    url: Option<String>,
}

#[derive(Serialize, Default)]
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreparedBatchEntry {
    offset: u64,
    byte_length: u64,
    cache_key: String,
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

/// The JS input for `build_site_db`: the whole IG working set, in memory.
#[derive(Deserialize)]
struct SiteDbInput {
    /// sushi-config.yaml text.
    config: String,
    /// FSH sources: project path -> text.
    fsh: std::collections::BTreeMap<String, String>,
    /// Predefined resources: `input/resources/**` path -> JSON body. May be empty.
    #[serde(default)]
    predefined: std::collections::BTreeMap<String, Value>,
    /// Site-content files (pagecontent/images/includes) the S6 augmentation reads:
    /// project-relative path (e.g. `input/pagecontent/index.md`) -> base64 bytes.
    /// Text files may be base64'd UTF-8; images are raw bytes.
    #[serde(default)]
    site_files: std::collections::BTreeMap<String, String>,
    /// Injected build timestamp (seconds since epoch) — genDate/genDay/date come
    /// from this, never a wall clock (determinism, §2c).
    build_epoch_secs: i64,
    /// project.liquidAssetDirs, relative to the IG root (cycle default:
    /// ["input/includes"]). May be omitted (defaults to that).
    #[serde(default)]
    liquid_asset_dirs: Vec<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    revision: Option<String>,
}

/// SiteBuild/site.db projection inputs after `compileProject` has established an
/// immutable compile revision in the session. Authored bodies are equality
/// assertions only: this operation remains deliberately unable to compile.
#[derive(Clone, Serialize, Deserialize)]
struct SiteDbFromCompileInput {
    config: String,
    #[serde(default)]
    fsh: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    predefined: std::collections::BTreeMap<String, Value>,
    #[serde(default)]
    site_files: std::collections::BTreeMap<String, String>,
    build_epoch_secs: i64,
    #[serde(default)]
    liquid_asset_dirs: Vec<String>,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default)]
    revision: Option<String>,
    /// External-builder contract. Omitted means the readable v1 compatibility
    /// projection; new editor callers request `cycle-site/v2` explicitly.
    #[serde(default)]
    target: Option<String>,
}

const PREPARED_GUIDE_CACHE_SCHEMA: &str = "prepared-guide-cache-key/v1";
const PREPARED_GUIDE_RECIPE: &str = "sushi.snapshot+site-semantics/v1";
const CLOSED_SITE_BUILD_CACHE_SCHEMA: &str = "closed-site-build-cache-key/v1";
const CYCLE_PROJECTION_RECIPE: &str = "site-build.cycle-projection/v2";

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
struct ClosedSiteBuildCachePayload<'a> {
    schema: &'static str,
    recipe: &'static str,
    prepared_guide_key: &'a site_build::Sha256Digest,
    render_target: &'a site_build::RenderTarget,
    diagnostics: &'a BTreeSet<site_build::BuildDiagnostic>,
    contract: &'a str,
}

fn closed_site_build_cache_key(
    prepared_guide_key: &site_build::Sha256Digest,
    render_target: &site_build::RenderTarget,
    diagnostics: &BTreeSet<site_build::BuildDiagnostic>,
    contract: &str,
) -> Result<site_build::Sha256Digest, site_build::CanonicalError> {
    site_build::sha256_canonical(&ClosedSiteBuildCachePayload {
        schema: CLOSED_SITE_BUILD_CACHE_SCHEMA,
        recipe: CYCLE_PROJECTION_RECIPE,
        prepared_guide_key,
        render_target,
        diagnostics,
        contract,
    })
}

const SITE_BUILD_CAS_TRANSPORT: &str = "site-build-cas/v1";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SiteBuildFromCompileResult {
    transport_version: String,
    site_build: site_build::ClosedSiteBuild,
    /// Digest -> base64 raw artifact bytes. This is transport only; semantic v2
    /// asset artifacts themselves contain raw bytes, never base64 JSON fields.
    objects: BTreeMap<String, String>,
    /// Temporary source compatibility for consumers that have not adopted the
    /// generic CAS map. Present only when the caller selects/omits v1.
    #[serde(skip_serializing_if = "Option::is_none")]
    site_db_json: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenStockBuildResult {
    handle: String,
    build_id: String,
    site_build: site_build::SiteBuild,
    pages: Vec<String>,
}

/// One explicit demand-discovery/result transition. `requested` is the typed
/// Need<ArtifactKey> set encountered by Liquid; `resolved` is the immutable
/// resolution batch installed in the successor. Failed/deferred observations
/// remain typed records and are never represented as successful page reads.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StockArtifactTransition {
    requested: Vec<site_build::ArtifactKey>,
    read: Vec<site_build::ArtifactKey>,
    resolved: Vec<site_build::ArtifactRecord>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RenderStockPageResult {
    html: String,
    handle: String,
    predecessor_build_id: String,
    build_id: String,
    site_build: site_build::ClosedSiteBuild,
    objects: BTreeMap<String, String>,
    transition: StockArtifactTransition,
    non_ready_fragments: usize,
}

fn encode_cas_objects(
    objects: BTreeMap<site_build::Sha256Digest, Vec<u8>>,
) -> BTreeMap<String, String> {
    objects
        .into_iter()
        .map(|(digest, bytes)| (digest.to_string(), base64_encode(&bytes)))
        .collect()
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
) -> Result<site_build::PackageCoordinate, String> {
    if candidates.len() == 1 {
        return Ok(candidates[0].clone());
    }
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
    matches.pop().ok_or_else(|| {
        format!(
            "build_site_build_from_compile: dependency {package_id}#{requested} matches none of the resolved coordinates: {}",
            candidates
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
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

fn stock_input_media_type(namespace: &str, name: &str) -> Result<String, String> {
    let media_type = match namespace {
        STOCK_RUNTIME_INPUT_NAMESPACE if name == "release-header.html" => "text/html",
        STOCK_PAGE_SOURCE_NAMESPACE
        | STOCK_SITE_DATA_NAMESPACE
        | STOCK_STAGED_INCLUDE_NAMESPACE
        | STOCK_TEMPLATE_INCLUDE_NAMESPACE => match std::path::Path::new(name)
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some("html" | "xhtml") => "text/html",
            Some("md") => "text/markdown",
            Some("json") => "application/json",
            Some("xml") => "application/xml",
            Some("yaml" | "yml") => "application/yaml",
            Some("csv") => "text/csv",
            Some("css") => "text/css",
            Some("js") => "text/javascript",
            Some("svg") => "image/svg+xml",
            Some("png") => "image/png",
            Some("gif") => "image/gif",
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("woff") => "font/woff",
            _ => "application/octet-stream",
        },
        _ => {
            return Err(format!(
                "renderStockPage: unknown stock input namespace {namespace} for {name}"
            ))
        }
    };
    Ok(media_type.to_string())
}

fn stock_inputs_from_pages<'a>(
    pages: impl IntoIterator<Item = &'a PageArtifactReadSet>,
    provenance: &site_build::ArtifactProvenance,
    semantic_reads: &BTreeSet<site_build::ReadDependency>,
) -> Result<Vec<StockInput>, String> {
    let mut captured = BTreeMap::<site_build::ArtifactKey, Vec<u8>>::new();
    for reads in pages {
        for key in reads.input_reads() {
            let values = reads.input_objects().get(key).ok_or_else(|| {
                format!("renderStockPage: input {key:?} was read without captured bytes")
            })?;
            if values.len() != 1 {
                return Err(format!(
                    "renderStockPage: input {key:?} changed during rendering"
                ));
            }
            let bytes = values.iter().next().expect("one captured input");
            if let Some(existing) = captured.get(key) {
                if existing != bytes {
                    return Err(format!(
                        "renderStockPage: input {key:?} changed between rendered pages"
                    ));
                }
            } else {
                captured.insert(key.clone(), bytes.clone());
            }
        }
    }
    captured
        .into_iter()
        .map(|(key, bytes)| {
            let site_build::ArtifactKey::Data { namespace, name } = &key else {
                return Err(format!(
                    "renderStockPage: page recorded non-Data input key {key:?}"
                ));
            };
            let media_type = stock_input_media_type(namespace, name)?;
            Ok(StockInput {
                key,
                bytes,
                media_type,
                provenance: provenance.clone(),
                reads: semantic_reads.clone(),
            })
        })
        .collect()
}

// The result/error envelope helpers now live in the shared `api_envelope` crate
// (imported above) — one implementation for the Session and the `fig` CLI.

// ===========================================================================
// Session — the preferred isolated engine handle, with grouped methods and the
// uniform envelope.
// ===========================================================================

/// The editor's engine session. Construct once; call methods per operation.
///
/// A regular `Session` owns its mutable engine state. `Session::global()` remains
/// as an explicit compatibility door onto the legacy process-global instance.
#[wasm_bindgen]
pub struct Session {
    /// `Some` for an isolated session; `None` only for `Session::global()`.
    engine: Option<RefCell<Engine>>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    fn with_engine<T>(&self, f: impl FnOnce(&mut Engine) -> T) -> T {
        match &self.engine {
            Some(engine) => f(&mut engine.borrow_mut()),
            None => with_engine(f),
        }
    }
}

#[wasm_bindgen]
impl Session {
    /// Create an isolated session handle.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Session {
        set_panic_hook();
        Session {
            engine: Some(RefCell::new(Engine::default())),
        }
    }

    /// The shared process-global compatibility session. New code should construct
    /// an isolated `Session` instead.
    pub fn global() -> Session {
        set_panic_hook();
        Session { engine: None }
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

    /// Atomically validate and mount a contiguous binary batch. `manifest_json`
    /// is `[{offset,byteLength,cacheKey}]`; ranges must cover `bytes` exactly.
    /// The envelope reports decode/validation and immutable-layer mount timings.
    #[wasm_bindgen(js_name = mountPreparedBatch)]
    pub fn mount_prepared_batch(&self, bytes: Vec<u8>, manifest_json: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "mountPreparedBatch",
            self.with_engine(|engine| engine.mount_prepared_batch(bytes, manifest_json)),
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

    /// Compile a project in memory. Envelope result: `{ resources, diagnostics,
    /// timings }`.
    pub fn compile(&self, files_json: &str, config: &str, predefined_json: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "compile",
            self.with_engine(|e| e.compile(files_json, config, predefined_json)),
        )
    }

    /// Compile one complete project revision, including its authored site-file
    /// manifest. The latter supplies page-folder names to IG export so downstream
    /// SiteBuild/site.db projections reuse this exact compile instead of rerunning
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

    /// Replace the local StructureDefinitions the next `snapshot` resolves bases
    /// against. Envelope result: `{ "count": <n> }`.
    #[wasm_bindgen(js_name = setLocalResources)]
    pub fn set_local_resources(&self, json: &str) -> String {
        set_panic_hook();
        envelope(
            "setLocalResources",
            self.with_engine(|e| e.set_local_resources(json))
                .map(|n| serde_json::json!({ "count": n })),
        )
    }

    /// Generate a snapshot for an inline SD JSON or a canonical URL/id/name.
    /// Envelope result: `{ snapshot, messages }`.
    pub fn snapshot(&self, input: &str) -> String {
        set_panic_hook();
        envelope_ser("snapshot", self.with_engine(|e| e.snapshot(input)))
    }

    /// Build the site.db row model from in-memory IG inputs. Envelope result: the
    /// row model object.
    #[wasm_bindgen(js_name = buildSiteDb)]
    pub fn build_site_db(&self, input_json: &str) -> String {
        set_panic_hook();
        envelope(
            "buildSiteDb",
            self.with_engine(|e| e.build_site_db(input_json)),
        )
    }

    /// Project the last `compileProject` result into the site.db compatibility
    /// model without accepting sources or invoking the compiler again.
    #[wasm_bindgen(js_name = buildSiteDbFromCompile)]
    pub fn build_site_db_from_compile(&self, input_json: &str) -> String {
        set_panic_hook();
        envelope(
            "buildSiteDbFromCompile",
            self.with_engine(|e| e.build_site_db_from_compile(input_json)),
        )
    }

    /// Produce a closed, callback-free Cycle SiteBuild plus its generic CAS
    /// transport. `cycle-site/v2` returns typed semantic roots and raw assets;
    /// v1 optionally retains `siteDbJson` for old consumers.
    /// `buildSiteDbFromCompile` remains a migration adapter.
    #[wasm_bindgen(js_name = buildSiteBuildFromCompile)]
    pub fn build_site_build_from_compile(&self, input_json: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "buildSiteBuildFromCompile",
            self.with_engine(|e| e.build_site_build_from_compile(input_json)),
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

    /// Mount (REPLACE) the site tree the render surface serves pages/includes
    /// from. `files_json`: `{ "<rel path>": "<text>" | {"b64": "<bytes>"} }`
    /// (rel paths: `en/index.md`, `_includes/…`, `_data/…`, `txcache/…`);
    /// `options_json`: `{ "activeTables": bool, "runUuid": "…", "merge": bool,
    /// "engineFirstIncludes": bool, "artifactResolution": bool }` or "".
    /// Envelope result: `{ "mounted": <count> }`.
    #[wasm_bindgen(js_name = mountSite)]
    pub fn mount_site(&self, files_json: &str, options_json: &str) -> String {
        set_panic_hook();
        envelope(
            "mountSite",
            self.with_engine(|e| e.mount_site(files_json, options_json))
                .map(|n| serde_json::json!({ "mounted": n })),
        )
    }

    /// Materialize a template `id#ver` chain from the MOUNTED bundle packages and
    /// merge the staged `template/` tree into the site tree — the driven template
    /// story (task #39). Fetch the template chain packages first via the SAME
    /// bundle path regular packages take (`resolveProject`/`mount`); this call then
    /// walks the `base` chain and materializes byte-exactly (the same bytes the
    /// parity gate proves). Envelope result: `{ "files": <count> }`.
    #[wasm_bindgen(js_name = mountTemplate)]
    pub fn mount_template(&self, coord: &str) -> String {
        set_panic_hook();
        envelope(
            "mountTemplate",
            self.with_engine(|e| e.mount_template(coord))
                .map(|n| serde_json::json!({ "files": n })),
        )
    }

    /// Synthesize the stock-template page shells + `_data` model from the current
    /// compile + mounted template and merge them into the site tree (task #45 —
    /// the source-driven replacement for the pre-baked `-stock.json`). Call after
    /// `compile()` + `mountTemplate()`, before rendering. Envelope result:
    /// `{ "pages": <shell count>, "data": <_data file count> }`.
    #[wasm_bindgen(js_name = produceStockSite)]
    pub fn produce_stock_site(&self) -> String {
        set_panic_hook();
        envelope(
            "produceStockSite",
            self.with_engine(|e| e.produce_stock_site())
                .map(|(pages, data)| serde_json::json!({ "pages": pages, "data": data })),
        )
    }

    /// Freeze the current compiled/template/site generation behind an explicit
    /// native-template SiteBuild handle. Later session mounts or compiles cannot
    /// change what this handle renders. Call after `produceStockSite` and the
    /// final authored overlay mount.
    #[wasm_bindgen(js_name = openStockBuild)]
    pub fn open_stock_build(&self, template_coord: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "openStockBuild",
            self.with_engine(|e| e.open_stock_build(template_coord)),
        )
    }

    /// Render one page from an explicit frozen stock-build handle and promote
    /// all newly discovered typed artifact needs through SiteBuild::successor.
    /// The returned handle names the immutable successor; callers never rely on
    /// an ambient "last site" or mutable fragment cache as an identity boundary.
    #[wasm_bindgen(js_name = renderStockPage)]
    pub fn render_stock_page(&self, handle: &str, name: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "renderStockPage",
            self.with_engine(|e| e.render_stock_page(handle, name)),
        )
    }

    /// Render one fragment (`ref` = `{Type}-{id}`, `kind` = the registered
    /// fragment kind, e.g. `snapshot`). Served through the session-shared typed
    /// artifact cache (the same map the page pass fills). Envelope result:
    /// `{ "html": "…" }`.
    #[wasm_bindgen(js_name = renderFragment)]
    pub fn render_fragment(&self, ref_: &str, kind: &str) -> String {
        set_panic_hook();
        envelope(
            "renderFragment",
            self.with_engine(|e| e.render_fragment(ref_, kind))
                .map(|h| serde_json::json!({ "html": h })),
        )
    }

    /// Render a page by output name (e.g. `index.html`). Envelope result:
    /// `{ "html": "…" }`.
    #[wasm_bindgen(js_name = renderPage)]
    pub fn render_page(&self, name: &str) -> String {
        set_panic_hook();
        envelope(
            "renderPage",
            self.with_engine(|e| e.render_page(name))
                .map(|h| serde_json::json!({ "html": h })),
        )
    }

    /// The renderable page names (sorted `stem.html`). Envelope result:
    /// `{ "pages": [ … ] }`.
    #[wasm_bindgen(js_name = listPages)]
    pub fn list_pages(&self) -> String {
        set_panic_hook();
        envelope(
            "listPages",
            self.with_engine(|e| e.list_pages())
                .map(|p| serde_json::json!({ "pages": p })),
        )
    }

    /// ContentApi: render a Liquid source against the session provider
    /// (engine-first includes + mounted `_includes`/`_data`), with caller
    /// globals from `data_json` (a JSON object). Envelope result: `{ "html" }`.
    #[wasm_bindgen(js_name = renderLiquid)]
    pub fn render_liquid(&self, source: &str, data_json: &str) -> String {
        set_panic_hook();
        envelope(
            "renderLiquid",
            self.with_engine(|e| e.render_liquid_src(source, data_json))
                .map(|h| serde_json::json!({ "html": h })),
        )
    }

    /// ContentApi: kramdown markdown, Jekyll `markdownify` semantics.
    /// `opts_json`: `{ "rougeWrappers": bool }` or "". Envelope result:
    /// `{ "html" }`.
    #[wasm_bindgen(js_name = renderMarkdown)]
    pub fn render_markdown(&self, md: &str, opts_json: &str) -> String {
        set_panic_hook();
        envelope(
            "renderMarkdown",
            self.with_engine(|e| e.render_markdown(md, opts_json))
                .map(|h| serde_json::json!({ "html": h })),
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

/// Parse `input/resources/**` JSON files out of the site_files map (base64 text)
/// into resource `Value`s — the example set the site.db orders after conformance.
fn collect_example_resources(
    site_files: &std::collections::BTreeMap<String, String>,
    operation: &str,
) -> Result<Vec<Value>, String> {
    let mut out = Vec::new();
    for (path, b64) in site_files {
        if !(path.starts_with("input/resources/") && path.ends_with(".json")) {
            continue;
        }
        let bytes = base64_decode(b64)
            .map_err(|error| format!("{operation}: invalid base64 in {path}: {error}"))?;
        let text = String::from_utf8(bytes)
            .map_err(|error| format!("{operation}: {path} is not UTF-8: {error}"))?;
        let value = serde_json::from_str::<Value>(&text)
            .map_err(|error| format!("{operation}: invalid JSON in {path}: {error}"))?;
        out.push(value);
    }
    Ok(out)
}

fn validate_predefined_site_overlap(
    predefined: &BTreeMap<String, Value>,
    site_files: &BTreeMap<String, String>,
    operation: &str,
) -> Result<(), String> {
    let predefined_paths: BTreeSet<&str> = predefined.keys().map(String::as_str).collect();
    let raw_paths: BTreeSet<&str> = site_files
        .keys()
        .filter(|path| path.starts_with("input/resources/") && path.ends_with(".json"))
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
            "{operation}: predefined and raw input/resources JSON paths differ (only predefined: {only_predefined:?}; only raw: {only_raw:?})"
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
    let kind = if path.starts_with("input/resources/") {
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
        .ok_or("build_site_build_from_compile: compiled revision has no ImplementationGuide")?;
    let project_id = ig
        .get("packageId")
        .or_else(|| ig.get("id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or("build_site_build_from_compile: ImplementationGuide has no packageId/id")?
        .to_string();
    let fhir_version = match ig.get("fhirVersion") {
        Some(Value::String(value)) => Some(value.as_str()),
        Some(Value::Array(values)) => values.first().and_then(Value::as_str),
        _ => None,
    }
    .filter(|value| !value.trim().is_empty())
    .ok_or("build_site_build_from_compile: ImplementationGuide has no fhirVersion")?
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

/// The `(resourceType, id)` set the IG marks as examples — an entry in
/// `definition.resource[]` with `exampleBoolean == true` or an `exampleCanonical`
/// (the publisher's example signal). Drives `is_example` in the stock producer.
/// FHIR conformance/definitional resource types the publisher lists as ARTIFACTS
/// (not examples). Anything else in the render set is an instance → an example.
const DEFINITIONAL_TYPES: &[&str] = &[
    "StructureDefinition",
    "ValueSet",
    "CodeSystem",
    "CapabilityStatement",
    "OperationDefinition",
    "SearchParameter",
    "ConceptMap",
    "NamingSystem",
    "StructureMap",
    "ExampleScenario",
    "GraphDefinition",
    "MessageDefinition",
    "CompartmentDefinition",
    "TerminologyCapabilities",
    "ImplementationGuide",
];

/// Synthesize a minimal ImplementationGuide from the render-set resources when the
/// real (publisher-generated) one is absent — the in-wasm equivalent of the IG the
/// disk build gets from `ig_export`. Produces the fields the site-producer reads:
/// `url`/`version`/`id`/`name` (IG context) and `definition.resource[]` (each
/// resource's `Type/id` reference, display `name`, and `exampleBoolean` flag).
/// Ordering follows the render set (compiled first, then predefined) — the same
/// order `Session.compile` assembles it in.
fn synthesize_ig(render_set: &[(PathBuf, Value)]) -> Value {
    let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);

    // Derive the IG canonical + version from any conformance resource's url/version:
    // a profile url is `<canonical>/<ResourceType>/<id>`, so strip the last two
    // segments to recover the IG canonical base.
    let mut canonical = String::new();
    let mut version = String::new();
    for (_, body) in render_set {
        let rt = body
            .get("resourceType")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !DEFINITIONAL_TYPES.contains(&rt) {
            continue;
        }
        if canonical.is_empty() {
            if let Some(url) = field(body, "url") {
                if let Some((base, _)) = url.rsplit_once(&format!("/{rt}/")) {
                    canonical = base.to_string();
                }
            }
        }
        if version.is_empty() {
            if let Some(v) = field(body, "version") {
                version = v;
            }
        }
        if !canonical.is_empty() && !version.is_empty() {
            break;
        }
    }
    let ig_id = canonical.rsplit('/').next().unwrap_or("ig").to_string();

    let mut resource_entries = Vec::new();
    for (_, body) in render_set {
        let rt = body
            .get("resourceType")
            .and_then(Value::as_str)
            .unwrap_or("");
        if rt.is_empty() || rt == "ImplementationGuide" {
            continue;
        }
        let id = body.get("id").and_then(Value::as_str).unwrap_or("");
        if id.is_empty() {
            continue;
        }
        // Display name: the resource's own title/name; instances rarely carry one.
        let name = field(body, "title")
            .or_else(|| field(body, "name"))
            .unwrap_or_else(|| format!("{rt}/{id}"));
        let is_example = !DEFINITIONAL_TYPES.contains(&rt);
        let mut entry = serde_json::Map::new();
        entry.insert(
            "reference".into(),
            serde_json::json!({ "reference": format!("{rt}/{id}") }),
        );
        entry.insert("name".into(), Value::String(name));
        if is_example {
            entry.insert("exampleBoolean".into(), Value::Bool(true));
        }
        resource_entries.push(Value::Object(entry));
    }

    serde_json::json!({
        "resourceType": "ImplementationGuide",
        "id": ig_id,
        "url": format!("{canonical}/ImplementationGuide/{ig_id}"),
        "version": version,
        "name": ig_id,
        "definition": { "resource": resource_entries },
    })
}

fn example_reference_set(ig: &Value) -> std::collections::HashSet<(String, String)> {
    let mut set = std::collections::HashSet::new();
    let Some(arr) = ig
        .get("definition")
        .and_then(|d| d.get("resource"))
        .and_then(Value::as_array)
    else {
        return set;
    };
    for r in arr {
        let is_example = r.get("exampleBoolean").and_then(Value::as_bool) == Some(true)
            || r.get("exampleCanonical").and_then(Value::as_str).is_some();
        if !is_example {
            continue;
        }
        if let Some(reference) = r
            .get("reference")
            .and_then(|x| x.get("reference"))
            .and_then(Value::as_str)
        {
            if let Some((rt, id)) = reference.split_once('/') {
                set.insert((rt.to_string(), id.to_string()));
            }
        }
    }
    set
}

fn implementation_guide_fhir_version(ig: &Value) -> Option<String> {
    match ig.get("fhirVersion") {
        Some(Value::String(value)) => Some(value.clone()),
        Some(Value::Array(values)) => values.first().and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
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
    /// Mount (REPLACE) the site tree: `files_json` = `{ "<rel path>": "<text>"
    /// | { "b64": "<bytes>" } }` with rel paths like `en/index.md`,
    /// `_includes/menu.xml`, `_data/pages.json`, `txcache/vs-externals.json`.
    /// `options_json` = `{ "activeTables": bool, "runUuid": "..." }` (or "").
    fn mount_site(&mut self, files_json: &str, options_json: &str) -> Result<usize, String> {
        let m: std::collections::BTreeMap<String, Value> = serde_json::from_str(files_json)
            .map_err(|e| format!("mountSite: bad files JSON: {e}"))?;
        let mut files = std::collections::HashMap::new();
        for (rel, v) in m {
            let bytes = match &v {
                Value::String(t) => t.as_bytes().to_vec(),
                Value::Object(o) => {
                    let b64 = o
                        .get("b64")
                        .and_then(|x| x.as_str())
                        .ok_or_else(|| format!("mountSite: {rel}: object without b64"))?;
                    base64_decode(b64).map_err(|e| format!("mountSite: {rel}: bad b64: {e}"))?
                }
                _ => return Err(format!("mountSite: {rel}: value must be text or {{b64}}")),
            };
            files.insert(
                PathBuf::from(format!("/site/{}", rel.trim_start_matches('/'))),
                bytes,
            );
        }
        let next_options = if options_json.trim().is_empty() {
            SiteOptions::default()
        } else {
            serde_json::from_str(options_json)
                .map_err(|e| format!("mountSite: bad options JSON: {e}"))?
        };
        let semantics_changed = !self.site_options.same_fragment_semantics(&next_options);
        // A replacement may remove or change an existing txcache. A merge only
        // affects semantic rendering when the overlay itself touches txcache;
        // ordinary pages/data/includes/assets are page-surface inputs.
        let txcache_changed = !next_options.merge
            || files
                .keys()
                .any(|path| path.starts_with(PathBuf::from("/site/txcache")));
        self.site_options = next_options;
        if self.site_options.merge {
            self.site_files.extend(files);
        } else {
            self.site_files = files;
        }
        let n = self.site_files.len();
        if semantics_changed || txcache_changed {
            self.invalidate_render_semantics();
        } else {
            self.invalidate_render_surface();
        }
        Ok(n)
    }

    /// Materialize a template `id#ver` chain from the MOUNTED bundle packages and
    /// merge the staged `template/` tree into the site tree — the wasm half of the
    /// driven template story (task #39). The host fetches the template chain
    /// packages via the SAME bundle path regular packages take (resolve → fetch →
    /// `mount`); Rust then walks the `base` chain and materializes with the loader
    /// (`package_store::template_loader`) — Rust decides, host fetches.
    ///
    /// The materialized tree is merged additively into the site tree: `includes/X`
    /// maps to `_includes/X` (so the render surface's include resolution serves the
    /// template's `template-page.html`/`fragment-*.html`); every other file mounts
    /// under `template/X` for reference. Envelope result: `{ "files": <count> }`.
    fn mount_template(&mut self, coord: &str) -> Result<usize, String> {
        let (source, cache_root, _packages) = self.source()?;
        let paths = package_store::template_loader::TemplatePaths::new(&cache_root);
        let tree = package_store::template_loader::materialize(&source, &paths, coord)
            .map_err(|e| format!("mountTemplate {coord}: {e}"))?;
        let n = tree.len();
        for (rel, bytes) in tree.into_files() {
            // includes/* -> _includes/* (the render surface's include dir); other
            // files under template/* for reference/assets.
            let mapped = match rel.strip_prefix("includes/") {
                Some(name) => format!("_includes/{name}"),
                None => format!("template/{rel}"),
            };
            self.site_files
                .insert(PathBuf::from(format!("/site/{mapped}")), bytes);
        }
        self.invalidate_render_surface();
        Ok(n)
    }

    /// Synthesize the stock-template page SHELLS + `_data` model from the CURRENT
    /// compile (the render set + mounted template) and merge them into the site
    /// tree — the source-driven replacement for the pre-baked `-stock.json`
    /// warm-start bundle (task #45). Requires a prior `compile()` (for the render
    /// set incl. the IG) and `mountTemplate()` (for `config.json` + `layouts/*`).
    ///
    /// Wiring over `site_producer::ProducerInputs::from_memory`:
    ///   * `config.json` ← the mounted `/site/template/config.json`;
    ///   * `layouts/*`  ← the mounted `/site/template/layouts/*` (keyed
    ///     `template/layouts/<name>`, the config-relative path the producer reads);
    ///   * resources    ← the last compile's render set (the IG is pulled out and
    ///     passed as `ig`); example-ness comes from the IG's
    ///     `definition.resource[].example*` markers (publisher-faithful);
    ///   * page-fragment includes ← the staged `/site/_includes/*` names (so
    ///     `pages.json` only emits `intro`/`notes` for fragments that exist).
    ///
    /// Merges the produced shells to `/site/<name>` and `_data` to
    /// `/site/_data/<name>`, drops the render state, and returns
    /// `(pages, data)` counts.
    fn produce_stock_site(&mut self) -> Result<(usize, usize), String> {
        // 1. Template config.json (mountTemplate stores template files under
        //    /site/template/<rel>).
        let cfg_bytes = self
            .site_files
            .get(&PathBuf::from("/site/template/config.json"))
            .ok_or(
                "produceStockSite: no template config at /site/template/config.json \
                 — call mountTemplate() first",
            )?;
        let config_json: Value = serde_json::from_slice(cfg_bytes)
            .map_err(|e| format!("produceStockSite: bad template config.json: {e}"))?;

        // 2. layouts/* -> "template/layouts/<name>" (the producer's LayoutSource::Map
        //    also accepts the template-root-relative "layouts/<name>" fallback).
        let mut layouts: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for (p, bytes) in &self.site_files {
            if let Ok(rel) = p.strip_prefix("/site/template/") {
                let rels = rel.to_string_lossy();
                if rels.starts_with("layouts/") {
                    if let Ok(txt) = String::from_utf8(bytes.clone()) {
                        layouts.insert(format!("template/{rels}"), txt);
                    }
                }
            }
        }
        if layouts.is_empty() {
            return Err(
                "produceStockSite: no template layouts mounted (template/layouts/*) \
                 — call mountTemplate() first"
                    .into(),
            );
        }

        // 3. Render set -> resources + IG. Example-ness from the IG's
        //    definition.resource example markers.
        //
        // The publisher's ImplementationGuide resource is a build artifact: FSH IGs
        // get it from disk-only `ig_export` (not run in-wasm), and predefined-resource
        // IGs (US Core/IPS) never ship it in `input/resources` (it too is generated).
        // So the render set usually has NO IG. When absent, synthesize a minimal IG
        // from the render-set resources themselves — enough to drive the producer
        // (definition.resource[] for ordering/titles/example flags + canonical/version
        // for the IG context). The native/disk path is unaffected (it reads the real
        // IG from the built tree); this fallback lives only in the wasm surface.
        let ig_candidates: Vec<&(PathBuf, Value)> = self
            .last_compiled
            .iter()
            .filter(|(_, v)| {
                v.get("resourceType").and_then(Value::as_str) == Some("ImplementationGuide")
            })
            .collect();
        let explicit: Vec<&(PathBuf, Value)> = ig_candidates
            .iter()
            .copied()
            .filter(|(path, _)| path.starts_with("/__ig__"))
            .collect();
        let primary_ig =
            match explicit.as_slice() {
                [primary] => Some(*primary),
                [] if ig_candidates.len() == 1 => Some(ig_candidates[0]),
                [] if ig_candidates.is_empty() => None,
                [] => return Err(
                    "produceStockSite: multiple ImplementationGuides without an explicit primary"
                        .into(),
                ),
                _ => {
                    return Err(
                        "produceStockSite: multiple explicit primary ImplementationGuides".into(),
                    )
                }
            };
        let ig_json = primary_ig
            .map(|(_, body)| body.clone())
            .unwrap_or_else(|| synthesize_ig(&self.last_compiled));
        let primary_ig_id = primary_ig
            .and_then(|(_, body)| body.get("id"))
            .and_then(Value::as_str);
        let example_refs = example_reference_set(&ig_json);

        let mut resources = Vec::new();
        for (path, body) in &self.last_compiled {
            let rt = body
                .get("resourceType")
                .and_then(Value::as_str)
                .unwrap_or("");
            if rt == "ImplementationGuide"
                && primary_ig_id.is_some()
                && body.get("id").and_then(Value::as_str) == primary_ig_id
            {
                continue;
            }
            let id = body.get("id").and_then(Value::as_str).unwrap_or("");
            let fname = path
                .file_name()
                .and_then(|n| n.to_str())
                .map(String::from)
                .unwrap_or_else(|| format!("{rt}-{id}.json"));
            let is_example = example_refs.contains(&(rt.to_string(), id.to_string()));
            if let Some(r) = site_producer::Resource::from_value(body.clone(), &fname, is_example) {
                resources.push(r);
            }
        }

        // 4. Staged page-fragment include filenames (for pages.json intro/notes).
        let mut page_includes: std::collections::HashSet<String> = std::collections::HashSet::new();
        for p in self.site_files.keys() {
            if let Ok(rel) = p.strip_prefix("/site/_includes") {
                if let Some(name) = rel.file_name().and_then(|n| n.to_str()) {
                    page_includes.insert(name.to_string());
                }
            }
        }

        // The editor renders via hl7.fhir.template, whose staged pages live under
        // `en/`; the shell FILE keys + pages.json KEYS must match that page.path.
        let inputs = site_producer::ProducerInputs::from_memory(
            resources,
            &config_json,
            layouts,
            &ig_json,
            page_includes,
            "en/",
        )
        .map_err(|e| format!("produceStockSite: {e:#}"))?;
        let out =
            site_producer::produce(&inputs).map_err(|e| format!("produceStockSite: {e:#}"))?;

        let (np, nd) = (out.pages.len(), out.data.len());
        for (name, body) in out.pages {
            self.site_files
                .insert(PathBuf::from(format!("/site/{name}")), body.into_bytes());
        }
        for (name, body) in out.data {
            self.site_files.insert(
                PathBuf::from(format!("/site/_data/{name}")),
                body.into_bytes(),
            );
        }
        // Stage the generated navbar so the layouts' `{% include menu.xml %}`
        // resolves (an absent include renders an empty navbar). Only overwrites
        // when we generated one; an IG-authored input/includes/menu.xml already
        // mounted under _includes stays if the config carried no `menu:`.
        if let Some(menu) = &self.menu_xml {
            self.site_files.insert(
                PathBuf::from("/site/_includes/menu.xml"),
                menu.clone().into_bytes(),
            );
        }
        self.invalidate_render_surface();
        Ok((np, nd))
    }

    /// Establish the immutable predecessor and freeze the exact render surface
    /// that belongs to it. The mounted tree digest is part of the predecessor
    /// identity because warm template trees are host-provided bytes rather than
    /// members of the compile resolver closure.
    fn open_stock_build(&mut self, template_coord: &str) -> Result<OpenStockBuildResult, String> {
        if template_coord.trim().is_empty() {
            return Err("openStockBuild: template coordinate is empty".into());
        }
        let project = self.last_project.clone().ok_or(
            "openStockBuild: compileProject has not established a complete source revision",
        )?;
        let (project_id, fhir_version) = compiled_ig_identity(&self.last_compiled)
            .map_err(|e| format!("openStockBuild: {e}"))?;
        let project_revision = self
            .site_build_project_revision(&project, &project_id)
            .map_err(|e| e.replace("build_site_build_from_compile", "openStockBuild"))?;
        let package_lock = self
            .site_build_package_lock(&project)
            .map_err(|e| e.replace("build_site_build_from_compile", "openStockBuild"))?;

        let tree_manifest = self
            .site_files
            .iter()
            .map(|(path, bytes)| {
                (
                    path.to_string_lossy().into_owned(),
                    site_build::ContentRef::of_bytes(bytes, None::<String>),
                )
            })
            .collect::<BTreeMap<_, _>>();
        let tree_digest = site_build::sha256_canonical(&tree_manifest)
            .map_err(|e| format!("openStockBuild: hash mounted tree: {e}"))?;
        let mut parameters = BTreeMap::from([
            ("contract".into(), "stock-site/v1".into()),
            ("templateCoordinate".into(), template_coord.to_string()),
            ("mountedTreeSha256".into(), tree_digest.to_string()),
            (
                "activeTables".into(),
                self.site_options.active_tables.to_string(),
            ),
            (
                "artifactResolution".into(),
                self.site_options.artifact_resolution.to_string(),
            ),
            (
                "engineFirstIncludes".into(),
                self.site_options.engine_first_includes.to_string(),
            ),
        ]);
        if let Some(run_uuid) = &self.site_options.run_uuid {
            parameters.insert("runUuid".into(), run_uuid.clone());
        }
        let provenance_attributes = parameters.clone();
        let build = site_build::SiteBuild::new(
            project_revision,
            package_lock,
            site_build::RenderTarget {
                renderer: site_build::ProducerRef::new(
                    "stock-template-wasm",
                    env!("CARGO_PKG_VERSION"),
                ),
                mode: site_build::RenderMode::NativeTemplate,
                fhir_version,
                // The warm browser template tree is authenticated by the exact
                // mounted-tree digest above. It may not have a mounted package
                // coordinate, so claiming one in PackageLock would be false.
                template: None,
                parameters,
            },
            site_build::RenderPlan::default(),
            site_build::ArtifactCatalog::from_records(Vec::new())
                .map_err(|e| format!("openStockBuild: artifact catalog: {e}"))?,
            site_build_diagnostics(&self.last_compile_diagnostics),
        )
        .map_err(|e| format!("openStockBuild: predecessor: {e}"))?;
        let state = self.render_state()?;
        let pages = state.list_pages();
        let handle = build.build_id().to_string();
        // The browser adapter has one active stock generation. Opening the next
        // generation explicitly retires its prior affine handle chain so large
        // frozen trees/page objects do not accumulate across live edits.
        self.stock_builds.clear();
        self.stock_builds.insert(
            handle.clone(),
            StockBuildRuntime {
                state,
                build: build.clone(),
                pages: BTreeMap::new(),
                provenance_attributes,
            },
        );
        Ok(OpenStockBuildResult {
            handle,
            build_id: build.build_id().to_string(),
            site_build: build,
            pages,
        })
    }

    fn render_stock_page(
        &mut self,
        handle: &str,
        name: &str,
    ) -> Result<RenderStockPageResult, String> {
        let runtime = self.stock_builds.get(handle).cloned().ok_or_else(|| {
            format!("renderStockPage: unknown or released stock build handle {handle}")
        })?;
        let (html, reads) = runtime
            .state
            .render_page_tracked_by_name(name)
            .map_err(|e| format!("renderStockPage {name}: {e}"))?;
        let mut rendered_pages = runtime.pages.clone();
        rendered_pages.insert(
            name.to_string(),
            StockRenderedPage {
                html: html.clone(),
                reads: reads.clone(),
            },
        );

        let producer =
            site_build::ProducerRef::new("stock-template-wasm", env!("CARGO_PKG_VERSION"));
        let provenance = |recipe: &str| site_build::ArtifactProvenance {
            producer: producer.clone(),
            recipe: recipe.to_string(),
            attributes: runtime.provenance_attributes.clone(),
        };
        let semantic_reads = all_compile_inputs(&runtime.build);
        let inputs = stock_inputs_from_pages(
            rendered_pages.values().map(|page| &page.reads),
            &provenance("capture-page-input"),
            &semantic_reads,
        )?;
        let pages = rendered_pages
            .iter()
            .map(|(path, page)| {
                Ok(StockPage {
                    path: site_build::SourcePath::parse(path.clone())
                        .map_err(|e| format!("renderStockPage: invalid page path {path}: {e}"))?,
                    outcome: StockPageOutcome::Ready {
                        bytes: page.html.as_bytes().to_vec(),
                        reads: page.reads.clone(),
                    },
                    provenance: provenance("render-page"),
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let successor = collect_stock_revision(
            &runtime.build,
            inputs,
            pages,
            Vec::new(),
            StockFragmentPolicy {
                provenance: provenance("render-publisher-fragment"),
                reads: semantic_reads,
            },
        )
        .map_err(|e| format!("renderStockPage: successor: {e}"))?;
        successor
            .verify()
            .map_err(|e| format!("renderStockPage: verify successor: {e}"))?;
        let closed = successor
            .site_build()
            .clone()
            .close()
            .map_err(|e| format!("renderStockPage: close successor: {e}"))?;
        let build_id = successor.site_build().build_id().to_string();
        let predecessor_build_id = successor.predecessor().to_string();
        let objects = successor
            .objects()
            .iter()
            .map(|(digest, object)| (digest.to_string(), base64_encode(object.bytes())))
            .collect();

        let mut resolution_keys = reads.input_reads().clone();
        resolution_keys.extend(reads.requested().iter().cloned());
        resolution_keys.insert(site_build::ArtifactKey::Page {
            path: site_build::SourcePath::parse(name.to_string())
                .map_err(|e| format!("renderStockPage: invalid page path {name}: {e}"))?,
        });
        let resolved = resolution_keys
            .iter()
            .filter_map(|key| successor.site_build().artifacts().get(key).cloned())
            .collect();
        let transition = StockArtifactTransition {
            requested: reads.requested().iter().cloned().collect(),
            read: reads.read().iter().cloned().collect(),
            resolved,
        };
        let non_ready_fragments = reads
            .observations()
            .values()
            .filter(|observation| matches!(observation, ArtifactObservation::NotReady { .. }))
            .count();

        // Successful transitions consume the predecessor handle and install
        // its immutable successor atomically. A failed render leaves the prior
        // handle available for retry.
        self.stock_builds.remove(handle);
        self.stock_builds.insert(
            build_id.clone(),
            StockBuildRuntime {
                state: runtime.state,
                build: successor.site_build().clone(),
                pages: rendered_pages,
                provenance_attributes: runtime.provenance_attributes,
            },
        );
        Ok(RenderStockPageResult {
            html,
            handle: build_id.clone(),
            predecessor_build_id,
            build_id,
            site_build: closed,
            objects,
            transition,
            non_ready_fragments,
        })
    }

    /// The lazily-(re)built render surface for the current generation.
    fn render_state(&mut self) -> Result<Rc<RenderState>, String> {
        if let Some(rs) = &self.render_state {
            return Ok(rs.clone());
        }
        let semantics = if let Some(semantics) = &self.render_semantics {
            semantics.clone()
        } else {
            let compiled = self.snapshot_complete_own()?;
            // ContentApi is intentionally usable over a mounted site tree with
            // no package store at all.  Project compiles still receive the
            // exact resolver closure captured for their revision; the `None`
            // branch is only the package-free standalone content surface.
            let packages = self
                .bundle
                .as_ref()
                .map(|_| {
                    self.source_for_current_revision()
                        .map(|(source, _, _)| source)
                })
                .transpose()?;
            let semantics = Rc::new(build_render_semantics(
                compiled,
                packages,
                &self.site_files,
                &self.site_options,
            )?);
            self.render_semantics = Some(semantics.clone());
            semantics
        };
        let rs = Rc::new(build_render_state_from_semantics(
            &semantics,
            &self.site_files,
            &self.site_options,
        )?);
        self.render_state = Some(rs.clone());
        Ok(rs)
    }

    /// Snapshot-complete the differential-only StructureDefinitions in the
    /// render set for the render surface's `/own` dir — the render layer walks
    /// `snapshot.element`. Resolves `baseDefinition` (and the type/extension/
    /// contentReference canonicals the walk touches) against the compile-bound
    /// exact package closure + the render set as locals — i.e. the SAME context
    /// the on-demand `snapshot` op uses (`build_context`), the publisher-faithful
    /// model the wasm-parity corpus gates (ips/mcode/sdc, full per-IG closure)
    /// prove byte-correct. SiteDb projection follows the same exact-closure
    /// rule: a predefined-resource IG may have profiles based on EXTERNAL bases
    /// (e.g. US Core's us-core-questionnaireresponse →
    /// sdc-questionnaireresponse), which a core-only context cannot resolve.
    ///
    /// SDs that already carry a snapshot pass through untouched. A per-SD
    /// snapshot failure is non-fatal: the differential body is left in place so
    /// the rest of the site still renders (that one page surfaces the editor's
    /// fragment-gap notice) rather than one bad profile blanking every page.
    /// With no differential-only SDs, this is a pure pass-through.
    fn snapshot_complete_own(&self) -> Result<Vec<(PathBuf, Value)>, String> {
        let needs: Vec<usize> = self
            .last_compiled
            .iter()
            .enumerate()
            .filter(|(_, (_, v))| {
                v.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition")
                    && v.get("snapshot").is_none()
            })
            .map(|(i, _)| i)
            .collect();
        if needs.is_empty() {
            return Ok(self.last_compiled.clone());
        }
        let ctx = self.build_context()?;
        let mut out = self.last_compiled.clone();
        for i in needs {
            let body = out[i].1.clone();
            if let Ok(snap) = snapshot_gen::generate_snapshot(body, &ctx, Default::default()) {
                out[i].1 = snap;
            }
        }
        Ok(out)
    }

    /// ContentApi: Liquid over the session provider (+ caller globals).
    fn render_liquid_src(&mut self, source: &str, data_json: &str) -> Result<String, String> {
        let rs = self.render_state()?;
        rs.render_liquid_src(source, data_json)
    }

    /// ContentApi: kramdown markdown (Jekyll semantics). `opts_json`:
    /// `{ "rougeWrappers": bool }` ("" = Jekyll markdownify defaults, wrappers ON —
    /// matching the page pass and the Liquid `markdownify` filter).
    fn render_markdown(&mut self, md: &str, opts_json: &str) -> Result<String, String> {
        let rouge = if opts_json.trim().is_empty() {
            true
        } else {
            let v: serde_json::Value = serde_json::from_str(opts_json)
                .map_err(|e| format!("renderMarkdown: bad options JSON: {e}"))?;
            v.get("rougeWrappers")
                .and_then(|x| x.as_bool())
                .unwrap_or(true)
        };
        Ok(render_md::render_with(
            md,
            &render_md::Options {
                rouge_wrappers: rouge,
                ..Default::default()
            },
        ))
    }

    fn render_fragment(&mut self, ref_: &str, kind: &str) -> Result<String, String> {
        let rs = self.render_state()?;
        rs.render_fragment(ref_, kind).map_err(|e| e.to_string())
    }

    fn render_page(&mut self, name: &str) -> Result<String, String> {
        let rs = self.render_state()?;
        rs.render_page_by_name(name)
    }

    fn list_pages(&mut self) -> Result<Vec<String>, String> {
        let rs = self.render_state()?;
        Ok(rs.list_pages())
    }
}

#[cfg(test)]
mod render_invalidation_tests {
    use super::*;

    fn compiled() -> Vec<(PathBuf, Value)> {
        vec![(
            PathBuf::from("ImplementationGuide-test.json"),
            serde_json::json!({
                "resourceType": "ImplementationGuide",
                "id": "test",
                "version": "1.0.0"
            }),
        )]
    }

    fn semantic_inputs() -> RenderSemanticInputs {
        RenderSemanticInputs {
            config: "id: test\nfhirVersion: 4.0.1\n".into(),
            fsh: BTreeMap::from([("input/fsh/test.fsh".into(), "Profile: Test".into())]),
            predefined: BTreeMap::new(),
            page_listing: BTreeMap::from([("input/pagecontent".into(), vec!["index.md".into()])]),
        }
    }

    fn semantics() -> Rc<RenderSemantics> {
        Rc::new(
            build_render_semantics(
                compiled(),
                None,
                &Default::default(),
                &SiteOptions::default(),
            )
            .expect("minimal render semantics"),
        )
    }

    #[test]
    fn ordinary_site_overlay_rebuilds_surface_but_reuses_fragment_engine() {
        let core = semantics();
        let mut engine = Engine {
            render_semantics: Some(core.clone()),
            ..Default::default()
        };

        engine
            .mount_site(
                r#"{"en/index.md":"---\ntitle: first\n---\nfirst"}"#,
                r#"{"merge":true}"#,
            )
            .unwrap();
        let first = engine.render_state().unwrap();
        assert!(Rc::ptr_eq(&first.engine, &core.engine));

        engine
            .mount_site(
                r#"{"en/second.md":"---\ntitle: second\n---\nsecond","template/config.json":"{}"}"#,
                r#"{"merge":true,"engineFirstIncludes":false,"artifactResolution":false}"#,
            )
            .unwrap();
        let second = engine.render_state().unwrap();

        assert!(!Rc::ptr_eq(&first, &second));
        assert!(Rc::ptr_eq(&first.engine, &second.engine));
        assert!(Rc::ptr_eq(&second.engine, &core.engine));
    }

    #[test]
    fn txcache_and_fragment_options_invalidate_semantic_core() {
        let mut txcache_engine = Engine {
            render_semantics: Some(semantics()),
            ..Default::default()
        };
        txcache_engine
            .mount_site(r#"{"txcache/vs-externals.json":"{}"}"#, r#"{"merge":true}"#)
            .unwrap();
        assert!(txcache_engine.render_semantics.is_none());

        let mut options_engine = Engine {
            render_semantics: Some(semantics()),
            ..Default::default()
        };
        options_engine
            .mount_site("{}", r#"{"merge":true,"activeTables":true}"#)
            .unwrap();
        assert!(options_engine.render_semantics.is_none());
    }

    #[test]
    fn prose_only_revision_can_retain_exact_semantic_core() {
        let core = semantics();
        let inputs = semantic_inputs();
        let compiled = compiled();
        let mut engine = Engine {
            last_compiled: compiled.clone(),
            last_render_semantic_inputs: Some(inputs.clone()),
            render_semantics: Some(core.clone()),
            last_project: Some(CompiledProjectRevision {
                config: inputs.config.clone(),
                fsh: inputs.fsh.clone(),
                predefined: inputs.predefined.clone(),
                site_files: BTreeMap::from([("input/pagecontent/index.md".into(), "old".into())]),
                resolved_packages: None,
            }),
            ..Default::default()
        };

        // compileProject captures the new raw prose revision separately. The
        // compiler-visible inputs and resulting render set are byte-for-byte
        // unchanged, so the existing semantic core remains authoritative.
        engine.replace_compiled_render_set(compiled, inputs);
        assert!(Rc::ptr_eq(engine.render_semantics.as_ref().unwrap(), &core));
    }

    #[test]
    fn compiler_visible_context_or_render_set_change_invalidates_semantic_core() {
        let baseline_inputs = semantic_inputs();
        let baseline_compiled = compiled();

        let mut changed_inputs = Vec::new();
        let mut config = baseline_inputs.clone();
        config.config.push_str("status: active\n");
        changed_inputs.push(("config", config));
        let mut fsh = baseline_inputs.clone();
        fsh.fsh
            .insert("input/fsh/next.fsh".into(), "Profile: Next".into());
        changed_inputs.push(("FSH", fsh));
        let mut predefined = baseline_inputs.clone();
        predefined.predefined.insert(
            "input/resources/Patient-p.json".into(),
            serde_json::json!({"resourceType": "Patient", "id": "p"}),
        );
        changed_inputs.push(("predefined", predefined));
        let mut pages = baseline_inputs.clone();
        pages
            .page_listing
            .get_mut("input/pagecontent")
            .unwrap()
            .push("new.md".into());
        changed_inputs.push(("page listing", pages));

        for (label, inputs) in changed_inputs {
            let mut engine = Engine {
                last_compiled: baseline_compiled.clone(),
                last_render_semantic_inputs: Some(baseline_inputs.clone()),
                render_semantics: Some(semantics()),
                ..Default::default()
            };
            engine.replace_compiled_render_set(baseline_compiled.clone(), inputs);
            assert!(
                engine.render_semantics.is_none(),
                "{label} change must invalidate semantics"
            );
        }

        let mut engine = Engine {
            last_compiled: baseline_compiled,
            last_render_semantic_inputs: Some(baseline_inputs.clone()),
            render_semantics: Some(semantics()),
            ..Default::default()
        };
        engine.replace_compiled_render_set(
            vec![(
                PathBuf::from("Patient-next.json"),
                serde_json::json!({"resourceType": "Patient", "id": "next"}),
            )],
            baseline_inputs,
        );

        assert!(engine.render_semantics.is_none());
        assert_eq!(engine.last_compiled.len(), 1);
    }

    #[test]
    fn stock_producer_uses_explicit_primary_and_keeps_additional_guides() {
        let mut engine = Engine {
            last_compiled: vec![
                (
                    PathBuf::from("/__compiled__/ImplementationGuide-aaa-example.json"),
                    serde_json::json!({
                        "resourceType": "ImplementationGuide",
                        "id": "aaa-example",
                        "packageId": "wrong.example",
                        "url": "https://wrong.example/ImplementationGuide/aaa-example",
                        "version": "9.0.0",
                        "fhirVersion": ["5.0.0"]
                    }),
                ),
                (
                    PathBuf::from("/__ig__/ImplementationGuide-primary.json"),
                    serde_json::json!({
                        "resourceType": "ImplementationGuide",
                        "id": "primary",
                        "packageId": "example.primary",
                        "url": "https://example.org/ImplementationGuide/primary",
                        "name": "Primary",
                        "version": "1.0.0",
                        "fhirVersion": ["4.0.1"],
                        "definition": { "resource": [] }
                    }),
                ),
            ],
            site_files: std::collections::HashMap::from([
                (
                    PathBuf::from("/site/template/config.json"),
                    br#"{"defaults":{}}"#.to_vec(),
                ),
                (
                    PathBuf::from("/site/template/layouts/unused.html"),
                    b"unused".to_vec(),
                ),
            ]),
            ..Default::default()
        };

        engine.produce_stock_site().unwrap();
        let fhir: Value =
            serde_json::from_slice(&engine.site_files[&PathBuf::from("/site/_data/fhir.json")])
                .unwrap();
        assert_eq!(fhir["ig"]["id"], "primary");
        let resources: Value = serde_json::from_slice(
            &engine.site_files[&PathBuf::from("/site/_data/resources.json")],
        )
        .unwrap();
        assert!(resources.get("ImplementationGuide/aaa-example").is_some());
    }

    #[test]
    fn package_context_replacement_invalidates_semantic_core() {
        let mut engine = Engine {
            render_semantics: Some(semantics()),
            ..Default::default()
        };
        engine
            .init(
                r#"[{"label":"example.package#1.0.0","files":{"package.json":"eyJuYW1lIjoiZXhhbXBsZS5wYWNrYWdlIiwidmVyc2lvbiI6IjEuMC4wIn0="}}]"#,
            )
            .unwrap();
        assert!(engine.render_semantics.is_none());
    }

    #[test]
    fn stock_handle_renders_frozen_tree_and_returns_closed_successor() {
        let config = "id: test\nfhirVersion: 4.0.1\n";
        let resolved = ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
            labels: Vec::new(),
        };
        let mut engine = Engine {
            last_compiled: vec![
                (
                    PathBuf::from("/__ig__/ImplementationGuide-test.json"),
                    serde_json::json!({
                        "resourceType": "ImplementationGuide",
                        "id": "test",
                        "packageId": "example.test",
                        "version": "1.0.0",
                        "fhirVersion": ["4.0.1"]
                    }),
                ),
                (
                    PathBuf::from("/__compiled__/StructureDefinition-test.json"),
                    serde_json::json!({
                        "resourceType": "StructureDefinition",
                        "id": "test",
                        "url": "http://example.org/StructureDefinition/test",
                        "name": "Test",
                        "status": "draft",
                        "fhirVersion": "4.0.1",
                        "kind": "resource",
                        "abstract": false,
                        "type": "Patient",
                        "baseDefinition": "http://hl7.org/fhir/StructureDefinition/Patient",
                        "derivation": "constraint",
                        "snapshot": { "element": [{ "id": "Patient", "path": "Patient" }] }
                    }),
                ),
            ],
            last_project: Some(CompiledProjectRevision {
                config: config.into(),
                fsh: BTreeMap::new(),
                predefined: BTreeMap::new(),
                site_files: BTreeMap::new(),
                resolved_packages: Some(resolved),
            }),
            site_files: std::collections::HashMap::from([(
                PathBuf::from("/site/en/index.html"),
                b"---\ntitle: Test\n---\n<p>frozen</p>{% include StructureDefinition-test-snapshot.xhtml %}".to_vec(),
            )]),
            ..Default::default()
        };

        let opened = engine.open_stock_build("hl7.fhir.template#1.0.0").unwrap();
        assert_eq!(opened.pages, vec!["en/index.html"]);
        engine
            .mount_site(
                r#"{"en/index.html":"---\ntitle: Test\n---\n<p>mutated</p>"}"#,
                "",
            )
            .unwrap();

        let rendered = engine
            .render_stock_page(&opened.handle, "en/index.html")
            .unwrap();
        assert!(rendered.html.contains("frozen"));
        assert!(!rendered.html.contains("mutated"));
        assert_eq!(rendered.predecessor_build_id, opened.build_id);
        assert_ne!(rendered.build_id, opened.build_id);
        rendered.site_build.site_build().verify().unwrap();
        assert_eq!(rendered.transition.requested.len(), 1);
        assert_eq!(rendered.transition.read.len(), 1);
        assert_eq!(rendered.non_ready_fragments, 0);
        assert!(rendered
            .transition
            .resolved
            .iter()
            .any(|record| matches!(record.key, site_build::ArtifactKey::Page { .. })));
        assert!(!rendered.objects.is_empty());
        let released = match engine.render_stock_page(&opened.handle, "en/index.html") {
            Err(error) => error,
            Ok(_) => panic!("consumed stock handle unexpectedly remained live"),
        };
        assert!(released.contains("unknown or released"));
        engine
            .render_stock_page(&rendered.handle, "en/index.html")
            .unwrap();
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
    fn prepared_batch_is_one_transaction_with_phase_metrics() {
        let package = |label: &str, marker: &str| {
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
                    ("marker.txt".into(), marker.as_bytes().to_vec()),
                ]),
            )
            .unwrap()
        };
        let a = package("a.pkg#1.0.0", "a");
        let b = package("b.pkg#1.0.0", "b");
        let a_bytes = a.encode();
        let b_bytes = b.encode();
        let mut bytes = a_bytes.clone();
        bytes.extend_from_slice(&b_bytes);
        let manifest = serde_json::json!([
            {"offset": 0, "byteLength": a_bytes.len(), "cacheKey": a.key.cache_key()},
            {"offset": a_bytes.len(), "byteLength": b_bytes.len(), "cacheKey": b.key.cache_key()},
        ]);
        let mut engine = Engine::default();
        engine.init("[]").unwrap();
        let outcome = engine
            .mount_prepared_batch(bytes.clone(), &manifest.to_string())
            .unwrap();
        assert_eq!(outcome.added, 2);
        assert_eq!(outcome.mounted, 2);
        assert_eq!(outcome.artifact_bytes, bytes.len() as u64);
        assert_eq!(
            outcome.manifest_json_bytes,
            manifest.to_string().len() as u64
        );
        assert_eq!(outcome.retained_blob_bytes, bytes.len() as u64);
        assert_eq!(outcome.member_body_copies, 0);
        assert!(outcome.indexed_members >= 4);
        assert!(
            outcome.manifest_parse_ms >= 0.0
                && outcome.decode_validate_ms >= 0.0
                && outcome.mount_ms >= 0.0
        );

        let conflict = package("a.pkg#1.0.0", "different");
        let fresh = package("c.pkg#1.0.0", "c");
        let conflict_bytes = conflict.encode();
        let fresh_bytes = fresh.encode();
        let mut failed_bytes = conflict_bytes.clone();
        failed_bytes.extend_from_slice(&fresh_bytes);
        let failed_manifest = serde_json::json!([
            {"offset": 0, "byteLength": conflict_bytes.len(), "cacheKey": conflict.key.cache_key()},
            {"offset": conflict_bytes.len(), "byteLength": fresh_bytes.len(), "cacheKey": fresh.key.cache_key()},
        ]);
        assert!(engine
            .mount_prepared_batch(failed_bytes, &failed_manifest.to_string())
            .unwrap_err()
            .contains("different content"));
        assert_eq!(engine.packages, vec!["a.pkg#1.0.0", "b.pkg#1.0.0"]);
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

        // Snapshot completion, on-demand snapshots, and RenderSemantics all use
        // this current-revision view rather than the newly enlarged cache.
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

        let state = engine.render_state().unwrap();
        assert_eq!(
            state
                .tree
                .read(&root.join(first).join("package/marker.txt"))
                .as_deref(),
            Some("first")
        );
        assert!(state
            .tree
            .read(&root.join(second).join("package/marker.txt"))
            .is_none());
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

        assert!(engine.mount_template("demo.template#1.0.0").unwrap() >= 3);
        assert_eq!(
            engine.site_files[&PathBuf::from("/site/_includes/demo.html")],
            b"nested include"
        );
        assert_eq!(
            engine.site_files[&PathBuf::from("/site/template/layouts/default.html")],
            b"layout"
        );
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
            .contains("only predefined"));
        let mut only_raw = different;
        only_raw.predefined.clear();
        assert!(engine
            .site_build_project_revision(&only_raw, "demo.ig")
            .unwrap_err()
            .contains("only raw"));
    }

    #[test]
    fn malformed_authored_resource_files_fail_loudly() {
        for (encoded, expected) in [
            ("not-base64".to_string(), "invalid base64"),
            ("/w==".to_string(), "not UTF-8"),
            (base64_encode(b"{"), "invalid JSON"),
        ] {
            let files = BTreeMap::from([("input/resources/Patient-p.json".into(), encoded)]);
            let error = collect_example_resources(&files, "test").unwrap_err();
            assert!(error.contains(expected), "{error}");
        }
    }

    #[test]
    fn package_lock_is_bound_to_resolved_config_and_intersects_declared_graph() {
        let config = "id: demo\nfhirVersion: 4.0.1\n";
        let core = "hl7.fhir.r4.core#4.0.1";
        let dep = "example.dep#1.0.0";
        let resolved = ResolvedPackages {
            config_sha256: site_build::Sha256Digest::of_bytes(config.as_bytes()),
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
    fn projection_is_bound_to_every_authored_compile_input() {
        let project = CompiledProjectRevision {
            config: "id: demo".into(),
            fsh: BTreeMap::from([("input/fsh/demo.fsh".into(), "Profile: Demo".into())]),
            predefined: BTreeMap::from([(
                "input/resources/Patient-p.json".into(),
                serde_json::json!({"resourceType": "Patient", "id": "p"}),
            )]),
            site_files: BTreeMap::new(),
            resolved_packages: None,
        };
        let engine = Engine {
            last_project: Some(project.clone()),
            ..Default::default()
        };
        let input = |fsh: BTreeMap<String, String>, predefined: BTreeMap<String, Value>| {
            SiteDbFromCompileInput {
                config: project.config.clone(),
                fsh,
                predefined,
                site_files: BTreeMap::new(),
                build_epoch_secs: 1,
                liquid_asset_dirs: Vec::new(),
                branch: None,
                revision: None,
                target: None,
            }
        };

        engine
            .validate_project_projection(
                &input(project.fsh.clone(), project.predefined.clone()),
                "test",
            )
            .unwrap();
        assert!(engine
            .validate_project_projection(
                &input(BTreeMap::new(), project.predefined.clone()),
                "test",
            )
            .unwrap_err()
            .contains("FSH files differ"));
        assert!(engine
            .validate_project_projection(&input(project.fsh, BTreeMap::new()), "test")
            .unwrap_err()
            .contains("predefined resources differ"));
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
            assets: Vec::new(),
        }
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
        let input = SiteDbFromCompileInput {
            config: config.into(),
            fsh: BTreeMap::new(),
            predefined: BTreeMap::new(),
            site_files: BTreeMap::new(),
            build_epoch_secs: 1,
            liquid_asset_dirs: Vec::new(),
            branch: None,
            revision: None,
            target: Some("cycle-site/v2".into()),
        };
        let revision = engine
            .site_build_project_revision(&project, "demo.ig")
            .unwrap();
        let lock = engine.site_build_package_lock(&project).unwrap();
        let key = engine
            .prepared_guide_cache_key(&input, &revision, &lock, "test")
            .unwrap();
        let prepared = minimal_prepared_guide();
        engine.prepared_guide_cache = Some(PreparedGuideCacheEntry {
            key: key.clone(),
            guide: prepared.clone(),
            site_db: None,
        });

        let (actual, db) = engine
            .site_model_from_compile(&input, "test", false, key.clone())
            .unwrap();
        assert_eq!(actual, prepared);
        assert!(db.is_none());
        assert_eq!(engine.derived_cache_hits.prepared_guide, 1);

        let mut changed = input.clone();
        changed.build_epoch_secs = 2;
        let changed_key = engine
            .prepared_guide_cache_key(&changed, &revision, &lock, "test")
            .unwrap();
        assert_ne!(changed_key, key);
        assert!(engine
            .site_model_from_compile(&changed, "test", false, changed_key)
            .unwrap_err()
            .contains("resolved closure has no required core package"));
        assert_eq!(engine.derived_cache_hits.prepared_guide, 1);
    }

    #[test]
    fn closed_build_cache_key_binds_target_diagnostics_and_contract() {
        let prepared = site_build::Sha256Digest::of_bytes(b"prepared");
        let target = site_build::RenderTarget {
            renderer: site_build::ProducerRef::new("cycle-site", "2"),
            mode: site_build::RenderMode::ExternalBuilder,
            fhir_version: "4.0.1".into(),
            template: None,
            parameters: BTreeMap::from([("contract".into(), "cycle-site/v2".into())]),
        };
        let diagnostics = BTreeSet::new();
        let baseline =
            closed_site_build_cache_key(&prepared, &target, &diagnostics, "cycle-site/v2").unwrap();
        let mut changed_target = target.clone();
        changed_target
            .parameters
            .insert("buildEpochSecs".into(), "2".into());
        assert_ne!(
            baseline,
            closed_site_build_cache_key(&prepared, &changed_target, &diagnostics, "cycle-site/v2")
                .unwrap()
        );
        let changed_diagnostics = BTreeSet::from([site_build::BuildDiagnostic::new(
            site_build::DiagnosticSeverity::Warning,
            "fixture",
            "warning",
        )]);
        assert_ne!(
            baseline,
            closed_site_build_cache_key(&prepared, &target, &changed_diagnostics, "cycle-site/v2")
                .unwrap()
        );
        assert_ne!(
            baseline,
            closed_site_build_cache_key(&prepared, &target, &diagnostics, "cycle-site/v1").unwrap()
        );
    }
}

// ===========================================================================
// Predefined-resource IG render surface — native gate (task #42).
//
// A predefined-resource IG (0 FSH; conformance authored under input/resources/**
// as DIFFERENTIAL-only SDs, e.g. US Core 9.0.0) must render LIVE: its profile
// pages need a real HierarchicalTableGenerator snapshot table, which means the
// render surface must (a) carry the predefined bodies in the render set and
// (b) snapshot-complete the differential-only SDs against the full mounted
// closure — including profiles whose base is EXTERNAL — never panicking on a
// binding shape the byte-parity corpus happens not to exercise.
//
// This test drives the EXACT render-surface path the fix changed
// (compile -> render set incl. predefined -> snapshot_complete_own ->
// render_fragment) over US Core 9.0.0's real closure + predefined SDs, sourced
// entirely from the local package cache (temp/fhir-home/.fhir/packages). It is
// network-free and skips if the cache is absent, like the sibling
// site_db_snapshot test.
//
// The predefined SDs are the published us.core#9.0.0 SDs with their `snapshot`
// REMOVED — i.e. the authored differential-only form the editor feeds from
// input/resources/**. Stripping the snapshot forces snapshot_complete_own to
// regenerate it against the closure, which is precisely the code path that
// panicked before the fix.
// ===========================================================================
#[cfg(test)]
mod predefined_render_gate {
    use super::*;
    use std::path::Path;

    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// The US Core 9.0.0 dependency closure, all present in the shared cache
    /// (verified network-free). Mounting the whole closure lets a profile whose
    /// base is external (us-core-questionnaireresponse -> sdc) snapshot-complete.
    const CLOSURE: &[&str] = &[
        "hl7.fhir.r4.core#4.0.1",
        "hl7.fhir.us.core#9.0.0",
        "hl7.fhir.uv.sdc#4.0.0",
        "hl7.fhir.uv.smart-app-launch#2.2.0",
        "hl7.fhir.uv.extensions.r4#5.3.0",
        "hl7.fhir.uv.tools.r4#1.1.2",
        "hl7.fhir.uv.xver-r5.r4#0.1.0",
        "hl7.terminology.r4#7.1.0",
        "us.cdc.phinvads#0.12.0",
    ];

    /// Only the conformance/index files a snapshot+render needs — never the bulky
    /// example instances. `BundleSource::read_dir` reflects exactly what we mount,
    /// so the package index build sees only these.
    fn is_wanted(name: &str) -> bool {
        matches!(name, ".index.json" | ".derived-index.json" | "package.json")
            || [
                "StructureDefinition-",
                "structuredefinition-",
                "ValueSet-",
                "valueset-",
                "CodeSystem-",
                "codesystem-",
                "ConceptMap-",
                "NamingSystem-",
                "ImplementationGuide-",
                "CapabilityStatement-",
                "SearchParameter-",
                "OperationDefinition-",
            ]
            .iter()
            .any(|p| name.starts_with(p))
    }

    fn mount_pkg(src: &mut BundleSource, cache: &Path, label: &str) -> bool {
        let dir = cache.join(label).join("package");
        if !dir.is_dir() {
            return false;
        }
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if is_wanted(&name) {
                if let Ok(bytes) = std::fs::read(&p) {
                    entries.push((name, bytes));
                }
            }
        }
        src.mount_package(label, entries);
        true
    }

    #[test]
    fn us_core_patient_renders_live_hierarchy_table() {
        let repo = repo();
        let cache = repo.join("temp/fhir-home/.fhir/packages");
        if !cache.join("hl7.fhir.us.core#9.0.0/package").is_dir() {
            eprintln!("skip: no us.core#9.0.0 in cache ({})", cache.display());
            return;
        }

        // ---- Mount the closure as a BundleSource (the render surface's package
        //      backend), conformance files only. ----
        let mut src = BundleSource::new();
        for label in CLOSURE {
            assert!(
                mount_pkg(&mut src, &cache, label),
                "closure package missing from cache: {label}"
            );
        }
        let cache_root = src.cache_root().to_path_buf();

        // ---- Predefined = us.core#9.0.0 conformance resources with `snapshot`
        //      stripped from every SD (the authored differential-only form the
        //      editor feeds from input/resources/**). ----
        let uscore = cache.join("hl7.fhir.us.core#9.0.0/package");
        let mut predefined = serde_json::Map::new();
        for e in std::fs::read_dir(&uscore).unwrap().flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            let lname = name.to_ascii_lowercase();
            let is_conf = ["structuredefinition-", "valueset-", "codesystem-"]
                .iter()
                .any(|pre| lname.starts_with(pre));
            if !is_conf || !name.ends_with(".json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else {
                continue;
            };
            let Ok(mut body) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            if body.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                body.as_object_mut().unwrap().remove("snapshot");
            }
            predefined.insert(format!("input/resources/{name}"), body);
        }
        assert!(
            predefined.len() > 80,
            "expected the full US Core conformance set, got {}",
            predefined.len()
        );
        let predefined_json = Value::Object(predefined).to_string();

        // Minimal sushi-config: id/canonical/fhirVersion + the real dependency
        // set so the compiler resolves the closure (0 FSH — predefined-only IG).
        let config = "\
id: hl7.fhir.us.core
canonical: http://hl7.org/fhir/us/core
name: USCore
title: US Core
status: active
version: 9.0.0
fhirVersion: 4.0.1
dependencies:
  hl7.fhir.uv.sdc: 4.0.0
  hl7.fhir.uv.smart-app-launch: 2.2.0
  hl7.fhir.uv.extensions.r4: 5.3.0
  hl7.fhir.uv.xver-r5.r4: 0.1.0
  us.cdc.phinvads: 0.12.0
";

        // ---- Drive the render surface EXACTLY as the Session worker does. ----
        let mut engine = Engine {
            bundle: Some(Rc::new(src)),
            cache_root,
            packages: CLOSURE.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };

        let compiled = engine
            .compile("{}", config, &predefined_json)
            .expect("compile predefined-only IG");
        // Fix #1: the predefined bodies land in the render set.
        let sd_count = engine
            .last_compiled
            .iter()
            .filter(|(_, v)| {
                v.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition")
            })
            .count();
        assert!(
            sd_count > 50,
            "render set must carry the predefined SDs; got {sd_count} (compiled {} resources)",
            compiled.resources.len()
        );

        // Fix #2: rendering the differential-only us-core-patient must produce a
        // real HierarchicalTableGenerator snapshot table — NO panic.
        let snap = engine
            .render_fragment("StructureDefinition-us-core-patient", "snapshot")
            .expect("us-core-patient snapshot fragment renders");
        assert!(
            snap.contains("class=\"hierarchy\""),
            "snapshot fragment must be a real hierarchy table; got:\n{}",
            &snap[..snap.len().min(400)]
        );
        // The table must have real rows (resource content), not an empty shell.
        assert!(
            snap.contains("us-core-patient") || snap.contains("Patient"),
            "hierarchy table must carry the profile's rows"
        );

        // The `tx` (terminology bindings) fragment is the one the browser panicked
        // on — it must render without aborting.
        let tx = engine
            .render_fragment("StructureDefinition-us-core-patient", "tx")
            .expect("us-core-patient tx fragment renders without panic");
        assert!(!tx.is_empty(), "tx fragment produced no output");

        // And the differential view (also part of the page) must not panic.
        let diff = engine
            .render_fragment("StructureDefinition-us-core-patient", "diff")
            .expect("us-core-patient diff fragment renders");
        assert!(
            diff.contains("class=\"hierarchy\""),
            "diff must be a hierarchy table"
        );

        eprintln!(
            "OK: us-core-patient live render — snapshot {} bytes, tx {} bytes, diff {} bytes; render-set SDs = {}",
            snap.len(),
            tx.len(),
            diff.len(),
            sd_count
        );

        // Whole-IG robustness: rendering EVERY predefined profile's snapshot/diff/tx
        // must never panic (the render surface builds the whole site — one bad
        // profile cannot be allowed to abort). Gaps (Err) are acceptable — a hard
        // panic (which is `unreachable`/abort in wasm) is not.
        let profile_ids: Vec<String> = engine
            .last_compiled
            .iter()
            .filter_map(|(_, v)| {
                (v.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition"))
                    .then(|| v.get("id").and_then(Value::as_str).map(String::from))
                    .flatten()
            })
            .collect();
        let mut rendered = 0usize;
        for id in &profile_ids {
            for kind in ["snapshot", "diff", "tx"] {
                // Err is fine (a documented gap); the point is no panic unwinds here.
                if engine
                    .render_fragment(&format!("StructureDefinition-{id}"), kind)
                    .is_ok()
                {
                    rendered += 1;
                }
            }
        }
        assert!(
            rendered > profile_ids.len(), // most fragments across most profiles render
            "expected most predefined profile fragments to render; only {rendered} of {} ok",
            profile_ids.len() * 3
        );
        eprintln!(
            "OK: whole-IG render smoke — {} profiles, {} fragments rendered, 0 panics",
            profile_ids.len(),
            rendered
        );
    }
}

// ===========================================================================
// A/B _data-sufficiency gate (task #45).
//
// Proves the source-driven `_data` model (site_producer) renders US Core pages
// identically to the known-good F0 publisher `_data`. Both sides share the SAME
// shells, `_includes`, compiled+snapshotted SDs and package closure — the ONLY
// variable is the structural `_data/*.json` (resources/structuredefinitions/
// pages/fhir/info/...). Any diff is therefore attributable to `_data`; we assert
// only the cited run-context classes remain (OID identifiers, artifact-page
// label numbering, cross-section prev/next nav — see docs/site-producer.md).
//
// Network-free; skips if the US Core package cache or F0 build tree is absent.
// Run: cargo test -q --release -p wasm_api -- --ignored ab_data
// ===========================================================================
#[cfg(test)]
mod ab_data_parity_gate {
    use super::*;
    use render_sd::tree::{FsTree, TreeSource};
    use std::collections::HashMap;
    use std::path::Path;

    const F0_USCORE: &str = "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds/us-core";

    fn repo() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    const CLOSURE: &[&str] = &[
        "hl7.fhir.r4.core#4.0.1",
        "hl7.fhir.us.core#9.0.0",
        "hl7.fhir.uv.sdc#4.0.0",
        "hl7.fhir.uv.smart-app-launch#2.2.0",
        "hl7.fhir.uv.extensions.r4#5.3.0",
        "hl7.fhir.uv.tools.r4#1.1.2",
        "hl7.fhir.uv.xver-r5.r4#0.1.0",
        "hl7.terminology.r4#7.1.0",
        "us.cdc.phinvads#0.12.0",
    ];

    fn is_wanted(name: &str) -> bool {
        matches!(name, ".index.json" | ".derived-index.json" | "package.json")
            || [
                "StructureDefinition-",
                "structuredefinition-",
                "ValueSet-",
                "valueset-",
                "CodeSystem-",
                "codesystem-",
                "ConceptMap-",
                "NamingSystem-",
                "ImplementationGuide-",
                "CapabilityStatement-",
                "SearchParameter-",
                "OperationDefinition-",
            ]
            .iter()
            .any(|p| name.starts_with(p))
    }

    fn mount_pkg(src: &mut BundleSource, cache: &Path, label: &str) -> bool {
        let dir = cache.join(label).join("package");
        if !dir.is_dir() {
            return false;
        }
        let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if is_wanted(&name) {
                if let Ok(bytes) = std::fs::read(&p) {
                    entries.push((name, bytes));
                }
            }
        }
        src.mount_package(label, entries);
        true
    }

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, out);
                } else {
                    out.push(p);
                }
            }
        }
    }

    /// Read the F0 `temp/pages` tree into a `/site/**` site_files map.
    fn load_site_files(pages_root: &Path, txcache: &Path) -> HashMap<PathBuf, Vec<u8>> {
        let mut site: HashMap<PathBuf, Vec<u8>> = HashMap::new();
        let mut all = Vec::new();
        walk(pages_root, &mut all);
        for f in all {
            let rel = f
                .strip_prefix(pages_root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            site.insert(
                PathBuf::from(format!("/site/{rel}")),
                std::fs::read(&f).unwrap(),
            );
        }
        if txcache.is_dir() {
            let mut tx = Vec::new();
            walk(txcache, &mut tx);
            for f in tx {
                let rel = f
                    .strip_prefix(txcache)
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                site.insert(
                    PathBuf::from(format!("/site/txcache/{rel}")),
                    std::fs::read(&f).unwrap(),
                );
            }
        }
        site
    }

    /// Classify a per-line diff as an ACCEPTED (cited) run-context class, or a
    /// hard content diff. Returns Some(class) if accepted, None if it must fail.
    fn classify(line: &str) -> Option<&'static str> {
        let l = line.trim();
        // OID identifiers row (publisher auto-assigned registry, not in source)
        if l.contains("Other Identifiers") || l.contains("OID:") || l.contains("UUID:") {
            return Some("oid-identifiers");
        }
        // artifact-page label numbering (CSS --heading-prefix var) + prev/next nav
        if l.contains("--heading-prefix") || l.contains("&lt;prev") || l.contains("next&gt;") {
            return Some("label-or-nav");
        }
        // footer/run-context (genDate/build year/publish-box placeholder)
        if l.contains("genDate") || l.contains("publish-box") || l.contains("Publish Box") {
            return Some("run-context");
        }
        None
    }

    fn render_both(
        compiled: &[(PathBuf, Value)],
        bundle: Option<Rc<BundleSource>>,
        site_a: &HashMap<PathBuf, Vec<u8>>,
        site_b: &HashMap<PathBuf, Vec<u8>>,
        page: &str,
    ) -> (String, String) {
        let opts = SiteOptions {
            active_tables: true,
            run_uuid: Some("00000000-0000-4000-8000-abdata000000".to_string()),
            merge: false,
            engine_first_includes: true,
            artifact_resolution: true,
        };
        let rs_a = build_render_state(compiled, bundle.clone(), site_a, &opts).expect("A state");
        let rs_b = build_render_state(compiled, bundle, site_b, &opts).expect("B state");
        (
            rs_a.render_page_by_name(page).expect("A render"),
            rs_b.render_page_by_name(page).expect("B render"),
        )
    }

    #[test]
    #[ignore = "needs the US Core F0 build tree + package cache; run explicitly"]
    fn ab_data_sufficiency_uscore() {
        let f0 = Path::new(F0_USCORE);
        let cache = repo().join("temp/fhir-home/.fhir/packages");
        if !f0.exists() || !cache.join("hl7.fhir.us.core#9.0.0/package").is_dir() {
            eprintln!(
                "skip: missing F0 tree ({}) or us.core cache ({})",
                f0.display(),
                cache.display()
            );
            return;
        }

        // ---- package closure ----
        let mut src = BundleSource::new();
        for label in CLOSURE {
            assert!(
                mount_pkg(&mut src, &cache, label),
                "closure pkg missing: {label}"
            );
        }
        let cache_root = src.cache_root().to_path_buf();
        let bundle = Some(Rc::new(src));

        // ---- compile the predefined US Core IG (differential SDs from input/resources) ----
        let mut predefined = serde_json::Map::new();
        let resdir = f0.join("input/resources");
        for e in std::fs::read_dir(&resdir).unwrap().flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if !name.ends_with(".json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else {
                continue;
            };
            let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            predefined.insert(format!("input/resources/{name}"), body);
        }
        let predefined_json = Value::Object(predefined).to_string();
        // also feed the fsh-generated IG (predefined-resource IGs keep the IG there)
        let mut fsh_predef = serde_json::Map::new();
        let gendir = f0.join("fsh-generated/resources");
        for e in std::fs::read_dir(&gendir).unwrap().flatten() {
            let p = e.path();
            let name = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();
            if !name.starts_with("ImplementationGuide") || !name.ends_with(".json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else {
                continue;
            };
            let Ok(body) = serde_json::from_slice::<Value>(&bytes) else {
                continue;
            };
            fsh_predef.insert(format!("input/resources/{name}"), body);
        }
        // Merge the IG into the predefined set so it lands in the render set.
        let mut all_predef: serde_json::Map<String, Value> =
            serde_json::from_str(&predefined_json).unwrap();
        all_predef.extend(fsh_predef);
        let predefined_json = Value::Object(all_predef).to_string();

        let config = "\
id: hl7.fhir.us.core
canonical: http://hl7.org/fhir/us/core
name: USCore
title: US Core
status: active
version: 9.0.0
fhirVersion: 4.0.1
dependencies:
  hl7.fhir.uv.sdc: 4.0.0
  hl7.fhir.uv.smart-app-launch: 2.2.0
  hl7.fhir.uv.extensions.r4: 5.3.0
  hl7.fhir.uv.xver-r5.r4: 0.1.0
  us.cdc.phinvads: 0.12.0
";
        let mut engine = Engine {
            bundle: bundle.clone(),
            cache_root,
            packages: CLOSURE.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        engine
            .compile("{}", config, &predefined_json)
            .expect("compile US Core");
        let compiled = engine
            .snapshot_complete_own()
            .expect("snapshot-complete render set");

        // ---- site trees: A = F0 _data, B = producer _data overlaid ----
        let pages_root = f0.join("temp/pages");
        let txcache = repo().join("temp/fhir-home"); // absent → no txcache; fine
        let site_a = load_site_files(&pages_root, &txcache);

        // Producer _data from source (gather_inputs over the F0 build dir).
        let inputs = site_producer::gather_inputs(f0).expect("gather_inputs");
        let produced = site_producer::produce(&inputs).expect("produce");
        let mut site_b = site_a.clone();
        for (name, body) in &produced.data {
            site_b.insert(
                PathBuf::from(format!("/site/_data/{name}")),
                body.clone().into_bytes(),
            );
        }

        // ---- render + classify ----
        let pages = ["StructureDefinition-us-core-patient.html", "index.html"];
        let mut hard_failures: Vec<String> = Vec::new();
        for page in pages {
            // ensure the shell + narrative source exists in the tree
            if FsTree.read(&pages_root.join(page)).is_none() {
                eprintln!("skip page (no shell): {page}");
                continue;
            }
            let (a, b) = render_both(&compiled, bundle.clone(), &site_a, &site_b, page);
            let a_lines: Vec<&str> = a.lines().collect();
            let b_lines: Vec<&str> = b.lines().collect();
            let mut classes: std::collections::BTreeMap<&str, usize> = Default::default();
            let mut matched = 0usize;
            let max = a_lines.len().max(b_lines.len());
            for i in 0..max {
                let al = a_lines.get(i).copied().unwrap_or("");
                let bl = b_lines.get(i).copied().unwrap_or("");
                if al == bl {
                    matched += 1;
                    continue;
                }
                // a diff at line i — classify by BOTH sides' content
                let cls = classify(al).or_else(|| classify(bl));
                match cls {
                    Some(c) => *classes.entry(c).or_default() += 1,
                    None => {
                        if hard_failures.len() < 12 {
                            hard_failures.push(format!(
                                "{page} L{i}:\n   A: {}\n   B: {}",
                                al.trim(),
                                bl.trim()
                            ));
                        }
                    }
                }
            }
            eprintln!(
                "A/B {page}: {}/{} lines identical; accepted-diff classes = {:?}",
                matched, max, classes
            );
        }
        assert!(
            hard_failures.is_empty(),
            "UNCLASSIFIED content diffs (not run-context/OID/label-nav):\n{}",
            hard_failures.join("\n")
        );
    }
}
