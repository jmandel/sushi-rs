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
//! s.compile(filesJson, config, predefinedJson);
//! s.snapshot(urlOrInlineSd);
//! s.buildSiteDb(inputJson);
//! s.expandValueSet(vsJson, resourcesJson);
//! s.resolveProject(config, versionIndexJson);
//! Session.version();                     // static
//! ```
//!
//! Every method returns a JSON string the Worker `JSON.parse`s. The envelope is
//! uniform:
//!   - success: `{ "apiVersion": 1, "ok": true,  "op": "<name>", "result": <payload> }`
//!   - failure: `{ "apiVersion": 1, "ok": false, "op": "<name>", "error": { "message": "…" } }`
//! Methods never throw for domain errors — they return `ok:false`; only a
//! genuinely unusable argument (non-string) surfaces as a JS exception.
//!
//! # Legacy free-function surface — DELETED (F6): `Session` is the only API.
//!
//! The original flat exports (`init` / `mount_bundles` / `compile` /
//! `set_local_resources` / `generate_snapshot` / `expand_enumerable` /
//! `build_site_db` / `resolve_project` / `version`) remain as thin wrappers over
//! the SAME shared [`Session`] state (the process-global session). They preserve
//! their exact historical output shapes byte-for-byte so the M2 editor + the
//! parity harness keep working unchanged; F6 migrates callers to [`Session`] and
//! then these can be deleted. New code must NOT use them.
//!
//! Everything runs synchronously in the Worker; the walk engine is the same code
//! the native gates exercise (proven byte-identical by `scripts/wasm-parity.sh`).
//!
//! ## Native build
//! The crate also builds on native targets (JS glue is inert there) so
//! `cargo test --workspace` links it. The real entry points are only meaningful
//! under `wasm32-unknown-unknown` + wasm-bindgen.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use package_store::{BundleSource, PackageSource};

mod render_surface;
use render_surface::{build_render_state, RenderState, SiteOptions};
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
struct SharedBundle(Rc<BundleSource>);

impl PackageSource for SharedBundle {
    fn read(&self, path: &std::path::Path) -> std::io::Result<Vec<u8>> {
        self.0.read(path)
    }
    fn read_dir(&self, path: &std::path::Path) -> std::io::Result<Vec<package_store::DirEntry>> {
        self.0.read_dir(path)
    }
    fn exists(&self, path: &std::path::Path) -> bool {
        self.0.exists(path)
    }
    fn is_dir(&self, path: &std::path::Path) -> bool {
        self.0.is_dir(path)
    }
    // write_new: default (read-only) — bundles ship the `.derived-index.json`.
}

// ---------------------------------------------------------------------------
// Engine — the mounted package source + last-compile locals. wasm is
// single-threaded, so all state lives behind one process-global handle. Both the
// `Session` object and the legacy free functions operate on the SAME `Engine`
// (there is exactly one engine; a `Session` is a typed door onto it).
//
// Every operation is an inherent method returning `Result<_, String>` — plain
// Rust errors, no `JsError` (which panics off-wasm). The two facades (Session +
// legacy fns) do the JS marshalling.
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
    /// Last `compile()` outputs `(synthetic path, body)`, indexed as local
    /// resources for snapshot base resolution.
    last_compiled: Vec<(PathBuf, Value)>,
    /// The mounted site tree (template statics + staged pagecontent + _data +
    /// _includes + optional txcache), keyed by virtual path under /site.
    site_files: std::collections::HashMap<PathBuf, Vec<u8>>,
    site_options: SiteOptions,
    /// `menu.xml` generated from the last `compile()`'s sushi-config `menu:` tree
    /// (the navbar is IG data, not template chrome). `produceStockSite` stages it
    /// into `_includes/` so the layouts' `{% include menu.xml %}` resolves.
    menu_xml: Option<String>,
    /// Lazily-built render surface; dropped whole on ANY state change
    /// (structural invalidation — the F6 "cache keyed off compile()
    /// generations" contract).
    render_state: Option<Rc<RenderState>>,
}

thread_local! {
    /// The one process-global engine. `Session::global()` and every legacy free
    /// function operate on this; a freshly-`new`d `Session` also points here (wasm
    /// is single-threaded and the editor keeps a single engine — a second
    /// independent engine has no use case yet, and sharing one keeps "one Engine
    /// handle" literally true).
    static ENGINE: RefCell<Engine> = RefCell::new(Engine::default());
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
        mount_into(&mut src, &parsed, &mut labels, "init")?;
        self.cache_root = src.cache_root().to_path_buf();
        self.bundle = Some(Rc::new(src));
        self.packages = labels;
        self.last_compiled.clear();
        self.render_state = None;
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
        let already: std::collections::BTreeSet<String> = labels.iter().cloned().collect();
        let fresh: Vec<BundleInput> = parsed
            .into_iter()
            .filter(|p| !already.contains(&p.label))
            .collect();
        // Fallible: on Err we return WITHOUT having touched our bundle/packages.
        mount_into(&mut src, &fresh, &mut labels, "mount_bundles")?;
        // Commit only after success.
        self.cache_root = src.cache_root().to_path_buf();
        self.bundle = Some(Rc::new(src));
        let total = labels.len() as u32;
        self.packages = labels;
        Ok(total)
    }

    /// The shared package source + cache root + package labels for a call. Cheap:
    /// an `Rc` refcount bump, so the mounted bytes are shared, never copied.
    fn source(&self) -> Result<(SharedBundle, PathBuf, Vec<String>), String> {
        let bundle = self
            .bundle
            .clone()
            .ok_or("engine not initialized: call init(bundles) first")?;
        Ok((SharedBundle(bundle), self.cache_root.clone(), self.packages.clone()))
    }

    /// Compile a project in memory. Returns the [`CompileResult`] payload and
    /// stashes the compiled resources as snapshot-resolution locals.
    fn compile(
        &mut self,
        files_json: &str,
        config: &str,
        predefined_json: &str,
    ) -> Result<CompileResult, String> {
        let (source, cache_root, _packages) = self.source()?;

        // FSH files: object -> Vec sorted by path (matches the disk walk order).
        let files_map: std::collections::BTreeMap<String, String> =
            serde_json::from_str(files_json).map_err(|e| format!("compile: bad files JSON: {e}"))?;
        let fsh_files: Vec<(String, String)> = files_map.into_iter().collect();

        // Predefined resources: object path -> body. Sorted by path so
        // `PredefinedPackage::load_from` sees the disk-equivalent order.
        let predefined: Vec<(PathBuf, Value)> = if predefined_json.trim().is_empty() {
            Vec::new()
        } else {
            let m: std::collections::BTreeMap<String, Value> = serde_json::from_str(predefined_json)
                .map_err(|e| format!("compile: bad predefined JSON: {e}"))?;
            m.into_iter().map(|(p, v)| (PathBuf::from(p), v)).collect()
        };

        let cache = cache_root.to_string_lossy().into_owned();
        // Keep the predefined bodies: the compiler consumes them for resolution
        // but does NOT re-emit them as `compiled` output. A predefined-resource IG
        // (0 FSH; conformance lives under input/resources/**, e.g. US Core / IPS)
        // would otherwise leave the render set holding only the ImplementationGuide
        // — every profile fragment misses. They ARE part of the publisher `output/`
        // tree, so they belong in the render set (/own) too.
        let predefined_for_render = predefined.clone();
        let (compiled, diagnostics) = compiler::build_project_in_memory_with_diagnostics(
            config, &fsh_files, predefined, source, &cache,
        )
        .map_err(|e| format!("compile failed: {e:#}"))?;

        // Stash the render set as snapshot-resolution locals: the FSH compile
        // output FIRST, then the predefined conformance/example resources keyed
        // by their publisher filename (`output/{Type}-{id}.json` parity). Both
        // land in the render surface's /own dir and act as base-resolution locals.
        self.render_state = None;
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
            render_set.push((PathBuf::from(format!("/__predefined__/{fname}")), body.clone()));
        }
        self.last_compiled = render_set;

        // Generate the navbar (menu.xml) from the sushi-config `menu:` tree so
        // produceStockSite can stage it into _includes/ — SUSHI writes this per-IG;
        // it is IG data, not template chrome (the template only supplies the
        // `{% include menu.xml %}` point).
        self.menu_xml = compiler::menu::menu_xml(config);

        let resources: Vec<CompiledResourceJs> = compiled
            .into_iter()
            .map(|r| {
                let rt = r.body.get("resourceType").and_then(|v| v.as_str()).map(str::to_string);
                let id = r.body.get("id").and_then(|v| v.as_str()).map(str::to_string);
                let url = r.body.get("url").and_then(|v| v.as_str()).map(str::to_string);
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

        Ok(CompileResult {
            resources,
            diagnostics,
            timings: Timings::default(),
        })
    }

    /// Set the "local" StructureDefinitions the next snapshot resolves bases
    /// against — the in-memory equivalent of the CLI's `--local-dir`. Replaces the
    /// local set from the last `compile()`. Returns the count.
    fn set_local_resources(&mut self, json: &str) -> Result<u32, String> {
        let map: std::collections::BTreeMap<String, Value> =
            serde_json::from_str(json).map_err(|e| format!("set_local_resources: bad JSON: {e}"))?;
        let locals: Vec<(PathBuf, Value)> = map
            .into_iter()
            .map(|(p, v)| (PathBuf::from(format!("/__local__/{p}")), v))
            .collect();
        let n = locals.len() as u32;
        self.render_state = None;
        self.last_compiled = locals;
        Ok(n)
    }

    /// Build a fresh `PackageContext` over the mounted packages + the last
    /// compile's local resources.
    fn build_context(&self) -> Result<snapshot_gen::PackageContext, String> {
        let (source, cache_root, packages) = self.source()?;
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

        Ok(match snapshot_gen::generate_snapshot(derived, &ctx, Default::default()) {
            Ok(v) => SnapshotResult {
                snapshot: Some(v),
                messages: Vec::new(),
            },
            Err(e) => SnapshotResult {
                snapshot: None,
                messages: vec![format!("{e:#}")],
            },
        })
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
        let input: SiteDbInput =
            serde_json::from_str(input_json).map_err(|e| format!("build_site_db: bad input JSON: {e}"))?;

        let (source, cache_root, _packages) = self.source()?;
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
        let mut page_dir_listing: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for path in input.site_files.keys() {
            for folder in ["pagecontent", "pages", "resource-docs"] {
                let prefix = format!("input/{folder}/");
                if let Some(rest) = path.strip_prefix(&prefix) {
                    // Only direct children participate in the page scan (not nested).
                    if !rest.is_empty() && !rest.contains('/') {
                        page_dir_listing
                            .entry(folder.to_string())
                            .or_default()
                            .push(rest.to_string());
                    }
                }
            }
        }

        let (conformance, ig_resource, _diagnostics) = compiler::build_project_in_memory_with_ig(
            &input.config,
            &fsh_files,
            predefined,
            source,
            &cache,
            page_dir_listing,
        )
        .map_err(|e| format!("build_site_db: compile failed: {e:#}"))?;

        // ---- S3: snapshot-complete each StructureDefinition against the compile. ----
        // Build the snapshot context EXACTLY as the native `site_db` pipeline does:
        // a `PackageContext` over ONLY the FHIR CORE package (r4/r5 core), plus the
        // just-compiled conformance SDs as locals so cross-profile bases resolve.
        // Loading the whole mounted closure here would pull extra type/extension
        // profiles into base resolution and inflate the snapshot vs the native
        // oracle — the native pipeline pins snapshotting to the single core package.
        let (source, cache_root, packages) = self.source()?;
        let core_package = pick_core_package(&packages).ok_or(
            "build_site_db: no FHIR core package (hl7.fhir.r{4,5}.core) mounted",
        )?;
        let mut ctx = snapshot_gen::PackageContext::new_with(source, &cache_root, &[core_package])
            .map_err(|e| format!("build_site_db: package context: {e:#}"))?;
        let locals: Vec<(PathBuf, Value)> = conformance
            .iter()
            .map(|r| (PathBuf::from(format!("/__compiled__/{}", r.filename)), r.body.clone()))
            .collect();
        ctx.load_local_resources(locals);

        let mut generated: Vec<Value> = Vec::new();
        for r in &conformance {
            if r.body.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
                let snap = snapshot_gen::generate_snapshot(r.body.clone(), &ctx, Default::default())
                    .map_err(|e| format!("build_site_db: snapshot {}: {e:#}", r.filename))?;
                generated.push(snap);
            } else {
                generated.push(r.body.clone());
            }
        }
        if let Some(ig) = &ig_resource {
            generated.push(ig.body.clone());
        } else {
            return Err(
                "build_site_db: no ImplementationGuide produced (FSHOnly config or missing id)"
                    .into(),
            );
        }

        // Predefined `input/resources/**` bodies are the examples (S5 loadResources).
        let examples: Vec<Value> = collect_example_resources(&input.site_files);

        // ---- Site-content VFS for S6 (pagecontent/images/includes), keyed /ig. ----
        let ig_root = PathBuf::from("/ig");
        let mut vfs: std::collections::BTreeMap<PathBuf, Vec<u8>> =
            std::collections::BTreeMap::new();
        for (path, b64) in &input.site_files {
            let bytes = base64_decode(b64)
                .map_err(|e| format!("build_site_db: bad base64 for {path}: {e}"))?;
            vfs.insert(ig_root.join(path), bytes);
        }

        let liquid_asset_dirs = if input.liquid_asset_dirs.is_empty() {
            vec!["input/includes".to_string()]
        } else {
            input.liquid_asset_dirs
        };

        // ---- S5/S6: assemble the row model. ----
        let outcome = site_db::build_from_inputs(&site_db::InMemoryInputs {
            generated: &generated,
            examples: &examples,
            sushi_config_yaml: &input.config,
            build_epoch_secs: input.build_epoch_secs,
            branch: input.branch,
            revision: input.revision,
            vfs,
            ig_root,
            liquid_asset_rel_dirs: liquid_asset_dirs,
        })
        .map_err(|e| format!("build_site_db: assemble rows: {e:#}"))?;

        serde_json::to_value(&outcome.db).map_err(|e| format!("build_site_db: serialize rows: {e}"))
    }

    /// Resolve a project's two package sets against the CURRENTLY MOUNTED bundles.
    /// Returns the [`package_store::ResolutionStep`]'s canonical JSON STRING (the
    /// exact `ResolutionStep::to_json()` bytes the legacy wrapper hands back
    /// verbatim; the Session path re-parses it into the envelope).
    fn resolve_project(&self, config: &str, version_index_json: &str) -> Result<String, String> {
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
        Ok(step.to_json())
    }
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
#[derive(Serialize)]
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

// The result/error envelope helpers now live in the shared `api_envelope` crate
// (imported above) — one implementation for the Session and the `fig` CLI.

// ===========================================================================
// Session — the preferred handle. One door onto the process-global engine, with
// grouped methods and the uniform envelope.
// ===========================================================================

/// The editor's engine session. Construct once; call methods per operation.
///
/// `Session` is a zero-sized typed handle onto the single process-global engine
/// (wasm is single-threaded). `new Session()` and `Session.global()` refer to the
/// same underlying state — a second independent engine has no use case yet.
#[wasm_bindgen]
#[derive(Default)]
pub struct Session {
    _private: (),
}

#[wasm_bindgen]
impl Session {
    /// Create a session handle (points at the shared process-global engine).
    #[wasm_bindgen(constructor)]
    pub fn new() -> Session {
        set_panic_hook();
        Session { _private: () }
    }

    /// The shared process-global session (identical to `new Session()`; provided
    /// so callers can name the intent).
    pub fn global() -> Session {
        Session::new()
    }

    /// Mount a set of prebuilt package bundles as the package cache, REPLACING any
    /// prior mount. `bundles_json`: `[{ "label": "id#ver", "files": { name: b64 }}]`.
    /// Envelope result: `{ "mounted": <count> }`.
    pub fn init(&self, bundles_json: &str) -> String {
        set_panic_hook();
        envelope(
            "init",
            with_engine(|e| e.init(bundles_json)).map(|n| serde_json::json!({ "mounted": n })),
        )
    }

    /// Mount ADDITIONAL bundles (additive, idempotent). Envelope result:
    /// `{ "mounted": <total-count> }`.
    pub fn mount(&self, bundles_json: &str) -> String {
        set_panic_hook();
        envelope(
            "mount",
            with_engine(|e| e.mount(bundles_json)).map(|n| serde_json::json!({ "mounted": n })),
        )
    }

    /// Compile a project in memory. Envelope result: `{ resources, diagnostics,
    /// timings }`.
    pub fn compile(&self, files_json: &str, config: &str, predefined_json: &str) -> String {
        set_panic_hook();
        envelope_ser(
            "compile",
            with_engine(|e| e.compile(files_json, config, predefined_json)),
        )
    }

    /// Replace the local StructureDefinitions the next `snapshot` resolves bases
    /// against. Envelope result: `{ "count": <n> }`.
    #[wasm_bindgen(js_name = setLocalResources)]
    pub fn set_local_resources(&self, json: &str) -> String {
        set_panic_hook();
        envelope(
            "setLocalResources",
            with_engine(|e| e.set_local_resources(json)).map(|n| serde_json::json!({ "count": n })),
        )
    }

    /// Generate a snapshot for an inline SD JSON or a canonical URL/id/name.
    /// Envelope result: `{ snapshot, messages }`.
    pub fn snapshot(&self, input: &str) -> String {
        set_panic_hook();
        envelope_ser("snapshot", with_engine(|e| e.snapshot(input)))
    }

    /// Build the site.db row model from in-memory IG inputs. Envelope result: the
    /// row model object.
    #[wasm_bindgen(js_name = buildSiteDb)]
    pub fn build_site_db(&self, input_json: &str) -> String {
        set_panic_hook();
        envelope("buildSiteDb", with_engine(|e| e.build_site_db(input_json)))
    }

    /// Tier-1 in-engine ValueSet expansion. Envelope result is the expansion
    /// payload (`{ ok, expansion, ... }` or `{ ok:false, notEnumerable }`).
    #[wasm_bindgen(js_name = expandValueSet)]
    pub fn expand_valueset(&self, valueset_json: &str, resources_json: &str) -> String {
        set_panic_hook();
        envelope(
            "expandValueSet",
            with_engine(|e| e.expand_valueset(valueset_json, resources_json)),
        )
    }

    /// Resolve a project's package sets against the mounted bundles. Envelope
    /// result: `{ compile_set, context_closure, missing, satisfied }`.
    #[wasm_bindgen(js_name = resolveProject)]
    pub fn resolve_project(&self, config: &str, version_index_json: &str) -> String {
        set_panic_hook();
        let payload = with_engine(|e| e.resolve_project(config, version_index_json)).and_then(|s| {
            serde_json::from_str::<Value>(&s).map_err(|e| format!("resolveProject: reparse: {e}"))
        });
        envelope("resolveProject", payload)
    }

    /// Mount (REPLACE) the site tree the render surface serves pages/includes
    /// from. `files_json`: `{ "<rel path>": "<text>" | {"b64": "<bytes>"} }`
    /// (rel paths: `en/index.md`, `_includes/…`, `_data/…`, `txcache/…`);
    /// `options_json`: `{ "activeTables": bool, "runUuid": "…" }` or "".
    /// Envelope result: `{ "mounted": <count> }`.
    #[wasm_bindgen(js_name = mountSite)]
    pub fn mount_site(&self, files_json: &str, options_json: &str) -> String {
        set_panic_hook();
        envelope(
            "mountSite",
            with_engine(|e| e.mount_site(files_json, options_json))
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
            with_engine(|e| e.mount_template(coord))
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
            with_engine(|e| e.produce_stock_site())
                .map(|(pages, data)| serde_json::json!({ "pages": pages, "data": data })),
        )
    }

    /// Render one fragment (`ref` = `{Type}-{id}`, `kind` = the registered
    /// fragment kind, e.g. `snapshot`). Served through the session-shared
    /// first-include-miss store (same map the page pass fills). Envelope
    /// result: `{ "html": "…" }`.
    #[wasm_bindgen(js_name = renderFragment)]
    pub fn render_fragment(&self, ref_: &str, kind: &str) -> String {
        set_panic_hook();
        envelope(
            "renderFragment",
            with_engine(|e| e.render_fragment(ref_, kind))
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
            with_engine(|e| e.render_page(name)).map(|h| serde_json::json!({ "html": h })),
        )
    }

    /// The renderable page names (sorted `stem.html`). Envelope result:
    /// `{ "pages": [ … ] }`.
    #[wasm_bindgen(js_name = listPages)]
    pub fn list_pages(&self) -> String {
        set_panic_hook();
        envelope(
            "listPages",
            with_engine(|e| e.list_pages()).map(|p| serde_json::json!({ "pages": p })),
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
            with_engine(|e| e.render_liquid_src(source, data_json))
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
            with_engine(|e| e.render_markdown(md, opts_json))
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

/// Decode + mount each bundle's base64 files under its label. Appends newly
/// mounted labels to `labels`.
fn mount_into(
    src: &mut BundleSource,
    parsed: &[BundleInput],
    labels: &mut Vec<String>,
    who: &str,
) -> Result<(), String> {
    for pkg in parsed {
        let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(pkg.files.len());
        for (name, b64) in &pkg.files {
            let bytes =
                base64_decode(b64).map_err(|e| format!("{who}: bad base64 for {name}: {e}"))?;
            entries.push((name.clone(), bytes));
        }
        src.mount_package(&pkg.label, entries);
        labels.push(pkg.label.clone());
    }
    Ok(())
}

/// Parse `input/resources/**` JSON files out of the site_files map (base64 text)
/// into resource `Value`s — the example set the site.db orders after conformance.
fn collect_example_resources(
    site_files: &std::collections::BTreeMap<String, String>,
) -> Vec<Value> {
    let mut out = Vec::new();
    for (path, b64) in site_files {
        if !(path.starts_with("input/resources/") && path.ends_with(".json")) {
            continue;
        }
        let Ok(bytes) = base64_decode(b64) else { continue };
        let Ok(text) = String::from_utf8(bytes) else { continue };
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            out.push(v);
        }
    }
    out
}

/// The `(resourceType, id)` set the IG marks as examples — an entry in
/// `definition.resource[]` with `exampleBoolean == true` or an `exampleCanonical`
/// (the publisher's example signal). Drives `is_example` in the stock producer.
/// FHIR conformance/definitional resource types the publisher lists as ARTIFACTS
/// (not examples). Anything else in the render set is an instance → an example.
const DEFINITIONAL_TYPES: &[&str] = &[
    "StructureDefinition", "ValueSet", "CodeSystem", "CapabilityStatement",
    "OperationDefinition", "SearchParameter", "ConceptMap", "NamingSystem",
    "StructureMap", "ExampleScenario", "GraphDefinition", "MessageDefinition",
    "CompartmentDefinition", "TerminologyCapabilities", "ImplementationGuide",
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
        let rt = body.get("resourceType").and_then(Value::as_str).unwrap_or("");
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
        let rt = body.get("resourceType").and_then(Value::as_str).unwrap_or("");
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

/// Pick the FHIR core package label (`hl7.fhir.r4.core#…` or `hl7.fhir.r5.core#…`)
/// from the mounted set — the single package the site.db snapshot context loads,
/// matching the native pipeline. Prefers R4 (the current corpus is R4) when both
/// are present; falls back to any `*.core` label.
fn pick_core_package(packages: &[String]) -> Option<String> {
    packages
        .iter()
        .find(|p| p.starts_with("hl7.fhir.r4.core#"))
        .or_else(|| packages.iter().find(|p| p.starts_with("hl7.fhir.r5.core#")))
        .or_else(|| packages.iter().find(|p| p.contains(".core#")))
        .cloned()
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
            files.insert(PathBuf::from(format!("/site/{}", rel.trim_start_matches('/'))), bytes);
        }
        self.site_options = if options_json.trim().is_empty() {
            SiteOptions::default()
        } else {
            serde_json::from_str(options_json)
                .map_err(|e| format!("mountSite: bad options JSON: {e}"))?
        };
        if self.site_options.merge {
            self.site_files.extend(files);
        } else {
            self.site_files = files;
        }
        let n = self.site_files.len();
        self.render_state = None;
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
        self.render_state = None;
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
            return Err("produceStockSite: no template layouts mounted (template/layouts/*) \
                 — call mountTemplate() first"
                .into());
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
        let ig_json = self
            .last_compiled
            .iter()
            .find(|(_, v)| {
                v.get("resourceType").and_then(Value::as_str) == Some("ImplementationGuide")
            })
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| synthesize_ig(&self.last_compiled));
        let example_refs = example_reference_set(&ig_json);

        let mut resources = Vec::new();
        for (path, body) in &self.last_compiled {
            let rt = body
                .get("resourceType")
                .and_then(Value::as_str)
                .unwrap_or("");
            if rt == "ImplementationGuide" {
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
        let mut page_includes: std::collections::HashSet<String> =
            std::collections::HashSet::new();
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
            self.site_files
                .insert(PathBuf::from(format!("/site/_data/{name}")), body.into_bytes());
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
        self.render_state = None;
        Ok((np, nd))
    }

    /// The lazily-(re)built render surface for the current generation.
    fn render_state(&mut self) -> Result<Rc<RenderState>, String> {
        if let Some(rs) = &self.render_state {
            return Ok(rs.clone());
        }
        let compiled = self.snapshot_complete_own()?;
        let rs = Rc::new(build_render_state(
            &compiled,
            self.bundle.clone(),
            &self.site_files,
            &self.site_options,
        )?);
        self.render_state = Some(rs.clone());
        Ok(rs)
    }

    /// Snapshot-complete the differential-only StructureDefinitions in the
    /// render set for the render surface's `/own` dir — the render layer walks
    /// `snapshot.element`. Resolves `baseDefinition` (and the type/extension/
    /// contentReference canonicals the walk touches) against the FULL mounted
    /// package closure + the render set as locals — i.e. the SAME context the
    /// on-demand `snapshot` op uses (`build_context`), the publisher-faithful
    /// model the wasm-parity corpus gates (ips/mcode/sdc, full per-IG closure)
    /// prove byte-correct. This is deliberately NOT the core-only pinning
    /// build_site_db uses: that matches the native `site_db` oracle, but a
    /// predefined-resource IG has profiles based on EXTERNAL bases (e.g. US
    /// Core's us-core-questionnaireresponse → sdc-questionnaireresponse), which
    /// core-only cannot resolve ("base not found: .../sdc/...").
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
            v.get("rougeWrappers").and_then(|x| x.as_bool()).unwrap_or(true)
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
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
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
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let lname = name.to_ascii_lowercase();
            let is_conf = ["structuredefinition-", "valueset-", "codesystem-"]
                .iter()
                .any(|pre| lname.starts_with(pre));
            if !is_conf || !name.ends_with(".json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let Ok(mut body) = serde_json::from_slice::<Value>(&bytes) else { continue };
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
        assert!(diff.contains("class=\"hierarchy\""), "diff must be a hierarchy table");

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
                "StructureDefinition-", "structuredefinition-", "ValueSet-", "valueset-",
                "CodeSystem-", "codesystem-", "ConceptMap-", "NamingSystem-",
                "ImplementationGuide-", "CapabilityStatement-", "SearchParameter-",
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
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
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
            let rel = f.strip_prefix(pages_root).unwrap().to_string_lossy().to_string();
            site.insert(PathBuf::from(format!("/site/{rel}")), std::fs::read(&f).unwrap());
        }
        if txcache.is_dir() {
            let mut tx = Vec::new();
            walk(txcache, &mut tx);
            for f in tx {
                let rel = f.strip_prefix(txcache).unwrap().to_string_lossy().to_string();
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
            eprintln!("skip: missing F0 tree ({}) or us.core cache ({})", f0.display(), cache.display());
            return;
        }

        // ---- package closure ----
        let mut src = BundleSource::new();
        for label in CLOSURE {
            assert!(mount_pkg(&mut src, &cache, label), "closure pkg missing: {label}");
        }
        let cache_root = src.cache_root().to_path_buf();
        let bundle = Some(Rc::new(src));

        // ---- compile the predefined US Core IG (differential SDs from input/resources) ----
        let mut predefined = serde_json::Map::new();
        let resdir = f0.join("input/resources");
        for e in std::fs::read_dir(&resdir).unwrap().flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if !name.ends_with(".json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let Ok(body) = serde_json::from_slice::<Value>(&bytes) else { continue };
            predefined.insert(format!("input/resources/{name}"), body);
        }
        let predefined_json = Value::Object(predefined).to_string();
        // also feed the fsh-generated IG (predefined-resource IGs keep the IG there)
        let mut fsh_predef = serde_json::Map::new();
        let gendir = f0.join("fsh-generated/resources");
        for e in std::fs::read_dir(&gendir).unwrap().flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            if !name.starts_with("ImplementationGuide") || !name.ends_with(".json") {
                continue;
            }
            let Ok(bytes) = std::fs::read(&p) else { continue };
            let Ok(body) = serde_json::from_slice::<Value>(&bytes) else { continue };
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
        engine.compile("{}", config, &predefined_json).expect("compile US Core");
        let compiled = engine.snapshot_complete_own().expect("snapshot-complete render set");

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
