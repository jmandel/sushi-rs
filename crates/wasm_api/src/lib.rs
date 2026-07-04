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
//! # Legacy free-function surface (DEPRECATED — kept for the live editor)
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

/// The result/error envelope version. Bump on any breaking change to the
/// envelope SHAPE (not payload contents).
const API_VERSION: u32 = 1;

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
        let (compiled, diagnostics) = compiler::build_project_in_memory_with_diagnostics(
            config, &fsh_files, predefined, source, &cache,
        )
        .map_err(|e| format!("compile failed: {e:#}"))?;

        // Stash the compiled resources as local resources for snapshot resolution.
        self.render_state = None;
        self.last_compiled = compiled
            .iter()
            .map(|r| {
                (
                    PathBuf::from(format!("/__compiled__/{}", r.filename)),
                    r.body.clone(),
                )
            })
            .collect();

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

// ---------------------------------------------------------------------------
// Envelope helpers (the ONE result shape + the ONE error shape for Session).
// ---------------------------------------------------------------------------

/// Serialize a session-method result into the uniform envelope string. Any
/// serialization failure degrades to a hand-built error envelope (never panics,
/// never throws).
fn envelope(op: &str, result: Result<Value, String>) -> String {
    let v = match result {
        Ok(payload) => serde_json::json!({
            "apiVersion": API_VERSION,
            "ok": true,
            "op": op,
            "result": payload,
        }),
        Err(message) => serde_json::json!({
            "apiVersion": API_VERSION,
            "ok": false,
            "op": op,
            "error": { "message": message },
        }),
    };
    // A serde_json::Value always serializes; the fallback is defensive only.
    serde_json::to_string(&v).unwrap_or_else(|_| {
        format!(
            "{{\"apiVersion\":{API_VERSION},\"ok\":false,\"op\":\"{op}\",\
             \"error\":{{\"message\":\"envelope serialize failed\"}}}}"
        )
    })
}

/// Serialize a `T: Serialize` result into the envelope (for typed payloads).
fn envelope_ser<T: Serialize>(op: &str, result: Result<T, String>) -> String {
    let as_value = result.and_then(|payload| {
        serde_json::to_value(&payload).map_err(|e| format!("{op}: serialize: {e}"))
    });
    envelope(op, as_value)
}

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
// Legacy free-function surface — DEPRECATED thin wrappers over the shared engine.
// They preserve their exact historical output shapes byte-for-byte (the M2 editor
// + parity harness depend on them). F6 migrates callers to `Session`, then these
// go away. `JsError` is reconstructed here so the wire type is unchanged.
// ===========================================================================

/// DEPRECATED: use `new Session().init(bundles)`. Mount prebuilt bundles as the
/// package cache. Returns the package count.
#[deprecated(note = "use Session::init (the session surface). Kept for the live editor; F6 removes it.")]
#[wasm_bindgen]
pub fn init(bundles_json: &str) -> Result<u32, JsError> {
    set_panic_hook();
    with_engine(|e| e.init(bundles_json)).map_err(|m| JsError::new(&m))
}

/// DEPRECATED: use `new Session().mount(bundles)`. Additive, idempotent mount.
/// Returns the total package count.
#[deprecated(note = "use Session::mount. Kept for the live editor; F6 removes it.")]
#[wasm_bindgen]
pub fn mount_bundles(bundles_json: &str) -> Result<u32, JsError> {
    set_panic_hook();
    with_engine(|e| e.mount(bundles_json)).map_err(|m| JsError::new(&m))
}

/// DEPRECATED: use `new Session().compile(...)`. Returns `{ resources,
/// diagnostics, timings }` (the RAW payload, not the session envelope).
#[deprecated(note = "use Session::compile. Kept for the live editor; F6 removes it.")]
#[wasm_bindgen]
pub fn compile(files_json: &str, config: &str, predefined_json: &str) -> Result<String, JsError> {
    set_panic_hook();
    let r = with_engine(|e| e.compile(files_json, config, predefined_json));
    match r {
        Ok(payload) => serde_json::to_string(&payload)
            .map_err(|e| JsError::new(&format!("compile: serialize: {e}"))),
        Err(m) => Err(JsError::new(&m)),
    }
}

/// DEPRECATED: use `new Session().setLocalResources(json)`. Returns the count.
#[deprecated(note = "use Session::set_local_resources. Kept for the live editor; F6 removes it.")]
#[wasm_bindgen]
pub fn set_local_resources(json: &str) -> Result<u32, JsError> {
    set_panic_hook();
    with_engine(|e| e.set_local_resources(json)).map_err(|m| JsError::new(&m))
}

/// DEPRECATED: use `new Session().snapshot(input)`. Returns `{ snapshot, messages }`
/// (the RAW payload, not the session envelope).
#[deprecated(note = "use Session::snapshot. Kept for the live editor; F6 removes it.")]
#[wasm_bindgen]
pub fn generate_snapshot(input: &str) -> Result<String, JsError> {
    set_panic_hook();
    let r = with_engine(|e| e.snapshot(input));
    match r {
        Ok(payload) => serde_json::to_string(&payload)
            .map_err(|e| JsError::new(&format!("serialize: {e}"))),
        Err(m) => Err(JsError::new(&m)),
    }
}

/// DEPRECATED: use `new Session().expandValueSet(vs, resources)`. Returns the RAW
/// expansion payload (`{ ok, ... }`), not the session envelope.
#[deprecated(note = "use Session::expand_valueset. Kept for the live editor; F6 removes it.")]
#[wasm_bindgen]
pub fn expand_enumerable(valueset_json: &str, resources_json: &str) -> Result<String, JsError> {
    set_panic_hook();
    let r = with_engine(|e| e.expand_valueset(valueset_json, resources_json));
    match r {
        Ok(payload) => serde_json::to_string(&payload)
            .map_err(|e| JsError::new(&format!("expand_enumerable: serialize: {e}"))),
        Err(m) => Err(JsError::new(&m)),
    }
}

/// DEPRECATED: use `new Session().buildSiteDb(input)`. Returns the RAW row-model
/// JSON, not the session envelope.
#[deprecated(note = "use Session::build_site_db. Kept for the live editor; F6 removes it.")]
#[wasm_bindgen]
pub fn build_site_db(input_json: &str) -> Result<String, JsError> {
    set_panic_hook();
    let r = with_engine(|e| e.build_site_db(input_json));
    match r {
        Ok(payload) => serde_json::to_string(&payload)
            .map_err(|e| JsError::new(&format!("build_site_db: serialize rows: {e}"))),
        Err(m) => Err(JsError::new(&m)),
    }
}

/// DEPRECATED: use `new Session().resolveProject(config, versionIndex)`. Returns
/// the RAW `ResolutionStep` JSON, not the session envelope.
#[deprecated(note = "use Session::resolve_project. Kept for the live editor; F6 removes it.")]
#[wasm_bindgen]
pub fn resolve_project(config: &str, version_index_json: &str) -> Result<String, JsError> {
    set_panic_hook();
    // Hand back the exact `ResolutionStep::to_json()` bytes (byte-identical to the
    // historical output the parity gate + editor consume).
    with_engine(|e| e.resolve_project(config, version_index_json)).map_err(|m| JsError::new(&m))
}

/// Engine version + build commit, as a JSON string `{ version, commit }`. (Not
/// deprecated — the same info `Session::version()` returns; a free accessor is
/// convenient and harmless.)
#[wasm_bindgen]
pub fn version() -> String {
    version_json()
}

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
        let n = files.len();
        self.site_files = files;
        self.render_state = None;
        Ok(n)
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

    /// Snapshot-complete the compiled StructureDefinitions for the render
    /// surface's `/own` dir — the render layer walks `snapshot.element`, and
    /// compile() emits differential-only SDs. Mirrors build_site_db's S3
    /// EXACTLY: a PackageContext over ONLY the FHIR core package, with the
    /// whole compile as locals so cross-profile bases resolve. SDs that
    /// already carry a snapshot pass through untouched. With no SDs (or no
    /// core package mounted — pure site smoke), this is a pass-through.
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
        let (source, cache_root, packages) = self.source()?;
        let core_package = pick_core_package(&packages)
            .ok_or("render surface: no FHIR core package (hl7.fhir.r{4,5}.core) mounted")?;
        let mut ctx = snapshot_gen::PackageContext::new_with(source, &cache_root, &[core_package])
            .map_err(|e| format!("render surface: package context: {e:#}"))?;
        ctx.load_local_resources(self.last_compiled.clone());
        let mut out = self.last_compiled.clone();
        for i in needs {
            let (path, body) = &out[i];
            let snap = snapshot_gen::generate_snapshot(body.clone(), &ctx, Default::default())
                .map_err(|e| format!("render surface: snapshot {}: {e:#}", path.display()))?;
            out[i].1 = snap;
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
