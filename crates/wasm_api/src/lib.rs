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
//! s.prepareProject(projectRevisionJson, generatorSpecJson);
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

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

use package_store::{
    BundleInput, BundleSource, PackageMountResult, PrepareMountResult, PreparedExport,
    PreparedStageResult,
};
use serde::Serialize;
use serde_json::Value;
use site_engine::PackageView as SharedBundle;
use site_engine::{
    GeneratorSpec as SharedGeneratorSpec, OutputCatalog as OutputCatalogResult, PackageEnvironment,
    ProjectRevision, ResolvedPackageClosure as ResolvedPackages, SiteEngine,
};
use wasm_bindgen::prelude::*;

/// The result/error envelope + apiVersion are the SHARED implementation
/// (`api_envelope`) — one schema for the Session and the `fig` CLI's `--json`.
use api_envelope::{envelope, envelope_ser, envelope_typed, API_VERSION};

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
    /// Exact prepared carriers from which both the resolver view and every
    /// SiteBuild PackageEnvironment are derived.
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
    /// Canonical target-neutral site executor. Package/compiler state above
    /// supplies captured inputs; all handle retention, output rendering,
    /// content reads, and finalization live behind this one shared executor.
    sites: SiteEngine,
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

#[derive(Clone, Debug)]
struct MountedPackage {
    prepared: package_store::PreparedPackage,
}

fn set_panic_hook() {
    #[cfg(target_family = "wasm")]
    console_error_panic_hook::set_once();
}

impl Engine {
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
        self.sites.clear_compilation();
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
            // requests must be resolved again before another project prepare.
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

    /// Commit a package batch prepared without holding the Session's mutable
    /// engine borrow. Keeping decode/normalize/encode outside this short commit
    /// lets package-resolution callbacks observe the prior complete generation
    /// instead of recursively borrowing a half-built transaction.
    fn commit_prepared_batch(
        &mut self,
        batch: PreparedMountBatch,
        base_generation: u64,
    ) -> Result<PrepareMountResult, String> {
        if self.bundle.is_none() {
            return Err("prepareAndMount: engine not initialized; call init() first".into());
        }
        if self.package_generation != base_generation {
            return Err(
                "prepareAndMount: package generation changed while artifacts were prepared".into(),
            );
        }
        // Do not expose artifacts from a transaction whose mount fails.
        let added = self.commit_prepared(batch.prepared, "prepareAndMount")?;
        self.prepared_exports.extend(batch.pending);
        Ok(PrepareMountResult {
            mounted: self.packages.len() as u32,
            added,
            artifacts: batch.artifacts,
            artifact_bytes: batch.artifact_bytes,
            prepared_members: batch.prepared_members,
            input_json_bytes: batch.input_json_bytes,
            base64_bytes: batch.base64_bytes,
            decoded_source_bytes: batch.decoded_source_bytes,
            normalized_bytes: batch.normalized_bytes,
            mount_member_body_copies: 0,
            json_parse_ms: batch.json_parse_ms,
            base64_decode_ms: batch.base64_decode_ms,
            normalization_ms: batch.normalization_ms,
            indexing_ms: batch.indexing_ms,
            artifact_encode_ms: batch.artifact_encode_ms,
            decode_validate_prepare_ms: batch.decode_validate_prepare_ms,
            mount_ms: 0.0,
        })
    }

    /// Retain only the compact exports from a package batch. The normalized
    /// package values are deliberately dropped; the host feeds each exported
    /// artifact into the existing multi-package prepared-mount transaction so
    /// a closure is still committed atomically without a closure-sized JSON
    /// string or duplicate decoded package graph.
    fn retain_prepared_artifacts(
        &mut self,
        batch: PreparedMountBatch,
        base_generation: u64,
    ) -> Result<PrepareMountResult, String> {
        if self.bundle.is_none() {
            return Err("prepareArtifacts: engine not initialized; call init() first".into());
        }
        if self.package_generation != base_generation {
            return Err(
                "prepareArtifacts: package generation changed while artifacts were prepared".into(),
            );
        }
        for label in batch.pending.keys() {
            if self.prepared_exports.contains_key(label) {
                return Err(format!(
                    "prepareArtifacts: pending artifact already exists for {label}"
                ));
            }
        }
        self.prepared_exports.extend(batch.pending);
        Ok(PrepareMountResult {
            mounted: self.packages.len() as u32,
            added: 0,
            artifacts: batch.artifacts,
            artifact_bytes: batch.artifact_bytes,
            prepared_members: batch.prepared_members,
            input_json_bytes: batch.input_json_bytes,
            base64_bytes: batch.base64_bytes,
            decoded_source_bytes: batch.decoded_source_bytes,
            normalized_bytes: batch.normalized_bytes,
            mount_member_body_copies: 0,
            json_parse_ms: batch.json_parse_ms,
            base64_decode_ms: batch.base64_decode_ms,
            normalization_ms: batch.normalization_ms,
            indexing_ms: batch.indexing_ms,
            artifact_encode_ms: batch.artifact_encode_ms,
            decode_validate_prepare_ms: batch.decode_validate_prepare_ms,
            mount_ms: 0.0,
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
        for package in &prepared {
            if !transaction.insert(package.label.clone()) {
                return Err(format!(
                    "{operation}: duplicate package label in one transaction: {}",
                    package.label
                ));
            }
            if let Some(existing) = self.package_materials.get(&package.label) {
                if existing.prepared.key != package.key
                    || existing.prepared.artifact_sha256() != package.artifact_sha256()
                {
                    return Err(format!(
                        "{operation}: package label {} is already mounted with different content",
                        package.label
                    ));
                }
            }
        }

        let mut source = (**base).clone(); // shallow: immutable layer Rc clones only
        let mut added = 0u32;
        let mut mounted_packages = Vec::new();
        for package in prepared {
            if self.package_materials.contains_key(&package.label) {
                continue;
            }
            let retained = package.clone();
            let mounted = package.mount_into(&mut source);
            debug_assert_eq!(mounted.label, retained.label);
            mounted_packages.push(retained);
            added += 1;
        }
        if added > 0 {
            let source = Rc::new(source);
            let root = source.cache_root().to_path_buf();
            for prepared in mounted_packages {
                let label = prepared.label.clone();
                self.packages.push(label.clone());
                self.package_materials
                    .insert(label, MountedPackage { prepared });
            }
            self.package_generation = self.package_generation.wrapping_add(1);
            self.cache_root = root;
            self.bundle = Some(source);
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
            SharedBundle::new(bundle, self.cache_root.clone(), None),
            self.cache_root.clone(),
            self.packages.clone(),
        ))
    }

    fn package_environment(&self) -> Result<PackageEnvironment, String> {
        let prepared = self
            .packages
            .iter()
            .map(|label| {
                self.package_materials
                    .get(label)
                    .map(|mounted| mounted.prepared.clone())
                    .ok_or_else(|| format!("mounted package {label} has no prepared carrier"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        PackageEnvironment::new(prepared)
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
            SharedBundle::new(bundle, self.cache_root.clone(), Some(allowed_labels)),
            self.cache_root.clone(),
            resolved.labels.clone(),
        ))
    }

    /// Package view for operations derived from the current prepare. A complete
    /// project revision remains bound to its captured resolver closure
    /// even if unrelated/template/other-version packages are mounted later.
    /// Internal non-project revisions have no such certificate and retain the
    /// historical all-mounted behavior for snapshot-only operations.
    fn source_for_current_revision(&self) -> Result<(SharedBundle, PathBuf, Vec<String>), String> {
        match self.sites.resolved_packages() {
            Some(resolved) => self.source_for_resolved(resolved),
            None => self.source(),
        }
    }

    /// Build a fresh `PackageContext` over the last complete project's exact
    /// resolved closure plus its local resources. Explicit legacy compile/local
    /// modes retain the historical all-mounted context.
    fn build_context(&self) -> Result<snapshot_gen::PackageContext, String> {
        let (source, cache_root, packages) = self.source_for_current_revision()?;
        let mut ctx = snapshot_gen::PackageContext::new_with(source, &cache_root, &packages)
            .map_err(|e| format!("package context: {e:#}"))?;
        ctx.load_local_resources(self.sites.compiled_resources().to_vec());
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

struct PreparedMountBatch {
    prepared: Vec<package_store::PreparedPackage>,
    pending: BTreeMap<String, Vec<u8>>,
    artifacts: Vec<PreparedExport>,
    artifact_bytes: u64,
    prepared_members: u64,
    input_json_bytes: u64,
    base64_bytes: u64,
    decoded_source_bytes: u64,
    normalized_bytes: u64,
    json_parse_ms: f64,
    base64_decode_ms: f64,
    normalization_ms: f64,
    indexing_ms: f64,
    artifact_encode_ms: f64,
    decode_validate_prepare_ms: f64,
}

/// Decode, normalize, index, and encode packages without borrowing the mutable
/// engine. This work can take seconds for large packages and invokes the host
/// clock for metrics; only the resulting immutable batch enters the commit.
fn prepare_package_batch(bundles_json: &str) -> Result<PreparedMountBatch, String> {
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
    Ok(PreparedMountBatch {
        prepared,
        pending,
        artifacts,
        artifact_bytes,
        prepared_members,
        input_json_bytes: bundles_json.len() as u64,
        base64_bytes,
        decoded_source_bytes,
        normalized_bytes,
        json_parse_ms: (parsed_at - started).max(0.0),
        base64_decode_ms,
        normalization_ms,
        indexing_ms,
        artifact_encode_ms,
        decode_validate_prepare_ms: (decoded - started).max(0.0),
    })
}

#[cfg(test)]
mod prepare_project_wire_tests {
    use super::*;

    #[test]
    fn site_failure_is_a_typed_error_with_the_successful_compilation() {
        let compilation = site_engine::CompilationOutcome {
            resources: Vec::new(),
            diagnostics: vec![site_engine::CompilationDiagnostic {
                severity: site_engine::CompilationDiagnosticSeverity::Warning,
                message: "compiled before generator failed".into(),
                file: Some("input/fsh/demo.fsh".into()),
                line: Some(3),
                owner_definition: None,
            }],
        };
        let result: Result<
            site_engine::PreparedProjectResult,
            site_engine::BuildError<site_engine::CompilationOutcome>,
        > = Err(site_engine::BuildError::new(
            site_engine::BuildOperation::Prepare,
            site_engine::BuildErrorPhase::Preparation,
            site_engine::BuildErrorCode::RendererFailed,
            "generator failed",
        )
        .with_successful_compilation(compilation));
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("site failure must remain an outer typed error"),
        };
        let value = serde_json::to_value(error).unwrap();

        assert_eq!(value["phase"], "preparation");
        assert_eq!(value["message"], "generator failed");
        assert_eq!(
            value["successfulCompilation"]["diagnostics"][0]["message"],
            "compiled before generator failed"
        );
    }

    #[test]
    fn diagnostic_owner_definition_uses_the_existing_definition_wire_shape() {
        let result = site_engine::CompilationOutcome {
            resources: Vec::new(),
            diagnostics: vec![site_engine::CompilationDiagnostic {
                severity: site_engine::CompilationDiagnosticSeverity::Error,
                message: "broken insert".into(),
                file: Some("input/fsh/demo.fsh".into()),
                line: Some(3),
                owner_definition: Some(site_engine::CompilationDefinition {
                    kind: site_engine::CompilationDefinitionKind::FshDeclaration,
                    path: "input/fsh/demo.fsh".into(),
                    line: 1,
                    column: 0,
                }),
            }],
        };
        let wire = serde_json::to_value(result).unwrap();
        assert_eq!(
            wire["diagnostics"][0]["ownerDefinition"],
            serde_json::json!({
                "kind": "fsh-declaration",
                "path": "input/fsh/demo.fsh",
                "line": 1,
                "column": 0
            })
        );
        assert!(wire["diagnostics"][0].get("owner_definition").is_none());
    }

    #[test]
    fn compile_failure_remains_an_outer_error_without_fake_compile_result() {
        let result: Result<
            site_engine::PreparedProjectResult,
            site_engine::BuildError<site_engine::CompilationOutcome>,
        > = Err(site_engine::BuildError::new(
            site_engine::BuildOperation::Prepare,
            site_engine::BuildErrorPhase::Compilation,
            site_engine::BuildErrorCode::CompileFailed,
            "compiler failed",
        ));
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("compile failure must remain an outer error"),
        };
        assert_eq!(error.code, site_engine::BuildErrorCode::CompileFailed);
        assert_eq!(error.message, "compiler failed");
    }
}

#[cfg(test)]
mod prepared_mount_tests {
    use super::*;

    #[test]
    fn compact_prepared_mount_does_not_inflate_package_bodies() {
        let package = package_store::PreparedPackage::prepare(
            "demo.package#1.0.0",
            BTreeMap::from([
                (
                    "package.json".into(),
                    br#"{"name":"demo.package","version":"1.0.0"}"#.to_vec(),
                ),
                (
                    "StructureDefinition-large.json".into(),
                    vec![b'x'; 2 * 1024 * 1024],
                ),
            ]),
        )
        .unwrap();
        let key = package.key.cache_key();
        let bytes = package.encode();
        let mut engine = Engine::default();
        engine.init("[]").unwrap();
        engine.begin_prepared_mount(1).unwrap();
        engine.stage_prepared_mount(bytes, &key).unwrap();
        let mounted = engine.commit_prepared_mount().unwrap();
        assert_eq!(mounted.added, 1);
        assert_eq!(mounted.compression.chunks_inflated, 0);
        assert_eq!(mounted.compression.raw_inflated_bytes, 0);
    }

    #[test]
    fn prepared_remount_rejects_nested_only_source_drift_atomically() {
        fn package(nested: &[u8]) -> package_store::PreparedPackage {
            package_store::PreparedPackage::prepare(
                "demo.package#1.0.0",
                BTreeMap::from([
                    (
                        "package.json".into(),
                        br#"{"name":"demo.package","version":"1.0.0"}"#.to_vec(),
                    ),
                    ("StructureDefinition-demo.json".into(), b"semantic".to_vec()),
                    ("template/private.txt".into(), nested.to_vec()),
                ]),
            )
            .unwrap()
        }

        let first = package(b"first");
        let second = package(b"second");
        assert_ne!(first.artifact_bytes(), second.artifact_bytes());
        assert_ne!(first.key.source_sha256, second.key.source_sha256);

        let first_key = first.key.clone();
        let mut engine = Engine::default();
        engine.init("[]").unwrap();
        engine.begin_prepared_mount(1).unwrap();
        engine
            .stage_prepared_mount(first.encode(), &first.key.cache_key())
            .unwrap();
        engine.commit_prepared_mount().unwrap();

        engine.begin_prepared_mount(1).unwrap();
        engine
            .stage_prepared_mount(second.encode(), &second.key.cache_key())
            .unwrap();
        assert!(engine
            .commit_prepared_mount()
            .unwrap_err()
            .contains("already mounted with different content"));
        assert_eq!(engine.packages, vec!["demo.package#1.0.0"]);
        assert_eq!(
            &engine.package_materials["demo.package#1.0.0"].prepared.key,
            &first_key
        );
    }
}

fn resolved_packages_from_step(
    config: &str,
    step: &package_store::ResolutionStep,
) -> Result<ResolvedPackages, String> {
    ResolvedPackages::from_resolution_step(config, step)
}

// ---------------------------------------------------------------------------
// JS-facing result shapes (serde -> JSON string the Worker JSON.parse()s, the
// simplest robust bindgen contract).
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct SnapshotResult {
    snapshot: Option<Value>,
    messages: Vec<String>,
}

struct CapturedProjectSource(Option<ProjectRevision>);

impl site_engine::ProjectSource for CapturedProjectSource {
    fn config(&mut self) -> Result<String, String> {
        self.0
            .as_ref()
            .map(|project| project.config.clone())
            .ok_or_else(|| "prepareProject: project revision was already consumed".into())
    }

    fn capture(
        &mut self,
        _packages: &PackageEnvironment,
        _resolved: &ResolvedPackages,
    ) -> Result<ProjectRevision, String> {
        self.0
            .take()
            .ok_or_else(|| "prepareProject: project revision was already consumed".into())
    }
}

struct MountedPackageProvider {
    resolved: Option<ResolvedPackages>,
    environment: Option<PackageEnvironment>,
}

impl site_engine::PackageProvider for MountedPackageProvider {
    fn resolve(
        &mut self,
        config: &str,
        _generator: &SharedGeneratorSpec,
    ) -> Result<ResolvedPackages, String> {
        let expected = site_build::Sha256Digest::of_bytes(config.as_bytes());
        self.resolved
            .as_ref()
            .filter(|resolved| resolved.config_sha256 == expected)
            .cloned()
            .ok_or_else(|| {
                "prepareProject: no satisfied package resolver fixpoint for these config bytes; resolve after the latest mount".into()
            })
    }

    fn environment(&mut self, _resolved: &ResolvedPackages) -> Result<PackageEnvironment, String> {
        self.environment
            .take()
            .ok_or_else(|| "prepareProject: package environment was already consumed".into())
    }
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
    active_operation: Cell<Option<&'static str>>,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    fn with_engine<T>(
        &self,
        operation: &'static str,
        f: impl FnOnce(&mut Engine) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut engine = self.engine.try_borrow_mut().map_err(|_| {
            format!(
                "{operation}: engine session is busy with reentrant operation {}",
                self.active_operation.get().unwrap_or("unknown")
            )
        })?;
        self.active_operation.set(Some(operation));
        let result = f(&mut engine);
        self.active_operation.set(None);
        result
    }

    fn with_build_engine<T>(
        &self,
        operation: &'static str,
        build_operation: site_engine::BuildOperation,
        f: impl FnOnce(&mut Engine) -> Result<T, site_engine::BuildError<()>>,
    ) -> Result<T, site_engine::BuildError<()>> {
        let mut engine = self.engine.try_borrow_mut().map_err(|_| {
            site_engine::BuildError::new(
                build_operation,
                site_engine::BuildErrorPhase::Lifecycle,
                site_engine::BuildErrorCode::Unavailable,
                format!(
                    "{operation}: engine session is busy with reentrant operation {}",
                    self.active_operation.get().unwrap_or("unknown")
                ),
            )
        })?;
        self.active_operation.set(Some(operation));
        let result = f(&mut engine);
        self.active_operation.set(None);
        result
    }
}

#[cfg(test)]
mod session_reentry_tests {
    use super::*;

    #[test]
    fn reentrant_session_use_is_a_typed_error_not_a_panic() {
        let session = Session::new();
        let _active = session.engine.borrow_mut();
        let envelope: Value = serde_json::from_str(&session.resolve_project("id: demo", ""))
            .expect("typed resolver envelope");
        assert_eq!(envelope["ok"], false);
        assert!(envelope["error"]["message"]
            .as_str()
            .unwrap()
            .contains("engine session is busy with reentrant operation unknown"));
    }
}

#[wasm_bindgen]
impl Session {
    /// Create an isolated session handle.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Session {
        set_panic_hook();
        let mut engine = Engine::default();
        engine.sites.set_clock(clock_ms);
        Session {
            engine: RefCell::new(engine),
            active_operation: Cell::new(None),
        }
    }

    /// Mount a set of prebuilt package bundles as the package cache, REPLACING any
    /// prior mount. `bundles_json`: `[{ "label": "id#ver", "files": { name: b64 }}]`.
    /// Envelope result: `{ "mounted": <count> }`.
    pub fn init(&self, bundles_json: &str) -> String {
        set_panic_hook();
        envelope(
            "init",
            self.with_engine("init", |engine| engine.init(bundles_json))
                .map(|n| serde_json::json!({ "mounted": n })),
        )
    }

    /// Mount ADDITIONAL bundles (additive, idempotent). Envelope result:
    /// `{ "mounted": <total-count> }`.
    pub fn mount(&self, bundles_json: &str) -> String {
        set_panic_hook();
        envelope(
            "mount",
            self.with_engine("mount", |engine| engine.mount(bundles_json))
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
            self.with_engine("mountPrepared", |engine| {
                engine.mount_prepared(bytes, expected_key)
            })
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
            self.with_engine("beginPreparedMount", |engine| {
                engine.begin_prepared_mount(expected_packages)
            })
            .map(|()| serde_json::json!({ "expectedPackages": expected_packages })),
        )
    }

    #[wasm_bindgen(js_name = stagePreparedMount)]
    pub fn stage_prepared_mount(&self, bytes: Vec<u8>, expected_key: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "stagePreparedMount",
            self.with_engine("stagePreparedMount", |engine| {
                engine.stage_prepared_mount(bytes, expected_key)
            }),
        )
    }

    #[wasm_bindgen(js_name = commitPreparedMount)]
    pub fn commit_prepared_mount(&self) -> String {
        set_panic_hook();
        envelope_ser(
            "commitPreparedMount",
            self.with_engine("commitPreparedMount", Engine::commit_prepared_mount),
        )
    }

    #[wasm_bindgen(js_name = abortPreparedMount)]
    pub fn abort_prepared_mount(&self) -> String {
        set_panic_hook();
        envelope(
            "abortPreparedMount",
            self.with_engine("abortPreparedMount", |engine| {
                Ok(engine.abort_prepared_mount())
            })
            .map(|aborted| serde_json::json!({ "aborted": aborted })),
        )
    }

    /// Cold-path bridge for the current inflated JSON/base64 package shape.
    /// Normalizes each package once, mounts it transactionally, and stages the
    /// exact `.fpp` artifact for zero-base64 transfer through `takePrepared`.
    #[wasm_bindgen(js_name = prepareAndMount)]
    pub fn prepare_and_mount(&self, bundles_json: &str) -> String {
        set_panic_hook();
        let result = (|| {
            let base_generation = self.with_engine("prepareAndMount.preflight", |engine| {
                if engine.bundle.is_none() {
                    return Err("prepareAndMount: engine not initialized; call init() first".into());
                }
                Ok(engine.package_generation)
            })?;

            // Package parsing, decoding, normalization, indexing, and encoding
            // deliberately run without a mutable Session borrow. In the browser
            // these phases invoke host clocks and may permit package-resolution
            // work to re-enter this Session; that work must see the previous
            // complete generation, never a half-built mount.
            let batch = prepare_package_batch(bundles_json)?;
            let mount_started = clock_ms();
            let mut result = self.with_engine("prepareAndMount.commit", move |engine| {
                engine.commit_prepared_batch(batch, base_generation)
            })?;
            result.mount_ms = (clock_ms() - mount_started).max(0.0);
            Ok(result)
        })();
        envelope_ser("prepareAndMount", result)
    }

    /// Convert one or a few cold raw packages into compact authenticated
    /// artifacts without mounting them. Hosts stage the one-shot exports into
    /// `beginPreparedMount`/`stagePreparedMount` and commit the complete closure
    /// once, preserving atomicity while bounding JSON and decoded-package memory.
    #[wasm_bindgen(js_name = prepareArtifacts)]
    pub fn prepare_artifacts(&self, bundles_json: &str) -> String {
        set_panic_hook();
        let result = (|| {
            let base_generation = self.with_engine("prepareArtifacts.preflight", |engine| {
                if engine.bundle.is_none() {
                    return Err(
                        "prepareArtifacts: engine not initialized; call init() first".into(),
                    );
                }
                Ok(engine.package_generation)
            })?;
            let batch = prepare_package_batch(bundles_json)?;
            self.with_engine("prepareArtifacts.retain", move |engine| {
                engine.retain_prepared_artifacts(batch, base_generation)
            })
        })();
        envelope_ser("prepareArtifacts", result)
    }

    /// Move one artifact staged by `prepareAndMount` into a JS `Uint8Array`.
    /// Metadata and errors for preparation remain in the uniform JSON envelope;
    /// a missing/twice-taken binary is surfaced as a JS exception.
    #[wasm_bindgen(js_name = takePrepared)]
    pub fn take_prepared(&self, label: &str) -> Result<Vec<u8>, wasm_bindgen::JsValue> {
        self.with_engine("takePrepared", |engine| engine.take_prepared(label))
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error))
    }

    /// Capture, compile, and prepare one project revision through the canonical
    /// Rust SiteEngine boundary. This is the one site-generation entry.
    #[wasm_bindgen(js_name = prepareProject)]
    pub fn prepare_project_site(
        &self,
        project_revision_json: &str,
        generator_spec_json: &str,
    ) -> String {
        set_panic_hook();
        let result = match self.engine.try_borrow_mut() {
            Ok(mut engine) => {
                self.active_operation.set(Some("prepareProject"));
                let result =
                    engine.prepare_project_site(project_revision_json, generator_spec_json);
                self.active_operation.set(None);
                result
            }
            Err(_) => Err(site_engine::BuildError::new(
                site_engine::BuildOperation::Prepare,
                site_engine::BuildErrorPhase::Lifecycle,
                site_engine::BuildErrorCode::Unavailable,
                format!(
                    "prepareProject: engine session is busy with reentrant operation {}",
                    self.active_operation.get().unwrap_or("unknown")
                ),
            )),
        };
        envelope_typed("prepareProject", result)
    }

    /// Generate a snapshot for an inline SD JSON or a canonical URL/id/name.
    /// Envelope result: `{ snapshot, messages }`.
    pub fn snapshot(&self, input: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "snapshot",
            self.with_engine("snapshot", |engine| engine.snapshot(input)),
        )
    }

    /// Complete, collision-checked output inventory for an immutable build.
    #[wasm_bindgen(js_name = outputs)]
    pub fn site_outputs(&self, handle: &str) -> String {
        set_panic_hook();
        envelope_typed(
            "outputs",
            self.with_build_engine("outputs", site_engine::BuildOperation::Outputs, |engine| {
                engine.site_outputs(handle)
            }),
        )
    }

    /// Materialize one declared output independently of every other path.
    #[wasm_bindgen(js_name = render)]
    pub fn render_site_output(&self, handle: &str, path: &str) -> String {
        set_panic_hook();
        envelope_typed(
            "render",
            self.with_build_engine("render", site_engine::BuildOperation::Render, |engine| {
                engine.render_site_output(handle, path)
            }),
        )
    }

    /// Return the canonical Rust SiteOutput. Native Publisher handles need no
    /// second argument; external Cycle handles supply their verified catalog
    /// and ContentRefs through the same no-argument `finalize` host operation.
    #[wasm_bindgen(js_name = finalize)]
    pub fn finalize_site(&self, handle: &str) -> String {
        set_panic_hook();
        envelope_typed(
            "finalize",
            self.with_build_engine(
                "finalize",
                site_engine::BuildOperation::Finalize,
                |engine| engine.finalize_site(handle),
            ),
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
        self.with_engine("readContent", |engine| {
            engine.read_site_content(handle, sha256)
        })
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error))
    }

    /// Private renderer bridge: bind one callback-free renderer and its exact
    /// output namespace to the retained Cycle build.
    #[wasm_bindgen(js_name = openRenderer)]
    pub fn open_site_renderer(
        &self,
        handle: &str,
        renderer_json: &str,
        output_schema: &str,
        options_json: &str,
        paths_json: &str,
    ) -> Result<(), wasm_bindgen::JsValue> {
        self.with_engine("openRenderer", |engine| {
            let renderer = serde_json::from_str(renderer_json)
                .map_err(|error| format!("openRenderer: invalid renderer identity: {error}"))?;
            let options = serde_json::from_str(options_json)
                .map_err(|error| format!("openRenderer: invalid renderer options: {error}"))?;
            let paths = serde_json::from_str(paths_json)
                .map_err(|error| format!("openRenderer: invalid output paths: {error}"))?;
            engine
                .sites
                .open_renderer(handle, renderer, output_schema.to_string(), options, paths)
        })
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error))
    }

    /// Private direct-binary renderer bridge. Rust authenticates and records
    /// the complete output file now; public `finalize` takes no plan.
    #[wasm_bindgen(js_name = admitOutput)]
    pub fn admit_site_output(
        &self,
        handle: &str,
        file_json: &str,
        bytes: Vec<u8>,
    ) -> Result<(), wasm_bindgen::JsValue> {
        self.with_engine("admitOutput", |engine| {
            let file: site_build::SiteOutputFile = serde_json::from_str(file_json)
                .map_err(|error| format!("admitOutput: invalid SiteOutputFile: {error}"))?;
            engine.sites.admit_output(handle, file, bytes)
        })
        .map_err(|error| wasm_bindgen::JsValue::from_str(&error))
    }

    /// Tier-1 in-engine ValueSet expansion. Envelope result is the expansion
    /// payload (`{ ok, expansion, ... }` or `{ ok:false, notEnumerable }`).
    #[wasm_bindgen(js_name = expandValueSet)]
    pub fn expand_valueset(&self, valueset_json: &str, resources_json: &str) -> String {
        set_panic_hook();
        envelope(
            "expandValueSet",
            self.with_engine("expandValueSet", |engine| {
                engine.expand_valueset(valueset_json, resources_json)
            }),
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
            .with_engine("resolveProject", |engine| {
                engine.resolve_project(config, version_index_json)
            })
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
            self.with_engine("resolveTemplate", |engine| {
                engine.resolve_template(coordinate)
            }),
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
        let package = package_store::PreparedPackage::prepare(&pkg.label, entries)
            .map_err(|error| format!("{who}: invalid package {}: {error:#}", pkg.label))?;
        let retained = package.clone();
        let mounted = package.mount_into(src);
        debug_assert_eq!(mounted.label, retained.label);
        labels.push(pkg.label.clone());
        package_materials.insert(pkg.label.clone(), MountedPackage { prepared: retained });
    }
    Ok(())
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
    fn resolve_template(
        &self,
        coordinate: &str,
    ) -> Result<site_engine::TemplateResolution, String> {
        if self.bundle.is_none() {
            return Ok(site_engine::TemplateResolution {
                satisfied: false,
                chain: Vec::new(),
                missing: Some(coordinate.to_string()),
            });
        }
        let environment = self.package_environment()?;
        let resolution = environment.resolve_template(coordinate)?;
        Ok(resolution)
    }

    fn prepare_project_site(
        &mut self,
        project_revision_json: &str,
        spec_json: &str,
    ) -> Result<
        site_engine::PreparedProjectResult,
        site_engine::BuildError<site_engine::CompilationOutcome>,
    > {
        let inputs: ProjectRevision =
            serde_json::from_str(project_revision_json).map_err(|error| {
                site_engine::BuildError::new(
                    site_engine::BuildOperation::Prepare,
                    site_engine::BuildErrorPhase::Input,
                    site_engine::BuildErrorCode::InvalidInput,
                    format!("prepareProject: invalid project revision: {error}"),
                )
            })?;
        let spec: SharedGeneratorSpec = serde_json::from_str(spec_json).map_err(|error| {
            site_engine::BuildError::new(
                site_engine::BuildOperation::Prepare,
                site_engine::BuildErrorPhase::Input,
                site_engine::BuildErrorCode::InvalidInput,
                format!("prepareProject: invalid generator specification: {error}"),
            )
        })?;
        let environment = self.package_environment().map_err(|message| {
            site_engine::BuildError::new(
                site_engine::BuildOperation::Prepare,
                site_engine::BuildErrorPhase::PackageTransport,
                site_engine::BuildErrorCode::Integrity,
                message,
            )
        })?;
        let mut source = CapturedProjectSource(Some(inputs));
        let mut packages = MountedPackageProvider {
            resolved: self.resolved_packages.clone(),
            environment: Some(environment),
        };
        let mut prepared = self
            .sites
            .prepare_project(&mut source, &mut packages, spec)?;
        let storage = serde_json::to_value(self.package_storage_metrics()).map_err(|error| {
            site_engine::BuildError::new(
                site_engine::BuildOperation::Prepare,
                site_engine::BuildErrorPhase::PackageTransport,
                site_engine::BuildErrorCode::Internal,
                format!("prepareProject: serialize package storage observations: {error}"),
            )
        })?;
        let metrics = storage
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, value)| value.as_f64().map(|number| (key.clone(), number)))
            .collect::<BTreeMap<_, _>>();
        prepared.events.push(site_engine::BuildEvent {
            operation: Some(site_engine::BuildOperation::Prepare),
            build_id: Some(prepared.site.build_id.clone()),
            stage: site_engine::BuildStage::BundleMount,
            label: None,
            bytes: None,
            total_bytes: None,
            message: "Measured retained package storage.".into(),
            fraction: None,
            from_cache: None,
            duration_ms: None,
            input_bytes: None,
            output_bytes: None,
            file_count: None,
            metrics: Some(metrics),
        });
        Ok(prepared)
    }

    fn site_outputs(
        &self,
        handle: &str,
    ) -> Result<OutputCatalogResult, site_engine::BuildError<()>> {
        self.sites.outputs(handle)
    }

    fn render_site_output(
        &mut self,
        handle: &str,
        path: &str,
    ) -> Result<site_build::ContentRef, site_engine::BuildError<()>> {
        self.sites.render(handle, path)
    }

    fn read_site_content(&self, handle: &str, digest: &str) -> Result<Vec<u8>, String> {
        self.sites.read_content(handle, digest)
    }

    fn finalize_site(
        &self,
        handle: &str,
    ) -> Result<site_build::SiteOutput, site_engine::BuildError<()>> {
        self.sites.finalize(handle)
    }
}
