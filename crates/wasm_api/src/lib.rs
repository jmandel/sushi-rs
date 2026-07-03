//! `wasm_api` — the wasm-bindgen JS surface for the FSH editor (wasm-editor-plan
//! P2). It keeps `wasm-bindgen` OUT of the core crates: the compiler and the
//! snapshot walk engine stay bindgen-free and native-tested; this crate is the
//! only place JS types are marshalled.
//!
//! # Surface (the Web Worker calls these)
//! - [`init`] — mount a set of prebuilt package bundles (the browser's package
//!   cache) into an in-memory [`package_store::BundleSource`]. Called once.
//! - [`compile`] — run the rust_sushi compiler in-memory over a `{path: text}`
//!   map of FSH sources + the `sushi-config.yaml` text, returning
//!   `{resources, diagnostics, timings}`.
//! - [`generate_snapshot`] — generate a validation-grade snapshot for a profile
//!   (given inline as an SD JSON or by canonical URL against the last compile +
//!   the mounted packages), returning `{snapshot, messages}`.
//! - [`version`] — the engine version + git commit.
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
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wasm_bindgen::prelude::*;

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
// Global engine state (wasm is single-threaded; a thread_local is the right
// shape). Holds the mounted package source + the last compile's resources so
// `generate_snapshot(url)` can resolve a just-compiled local profile.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Engine {
    bundle: Option<SharedBundle>,
    cache_root: PathBuf,
    /// The `<id>#<ver>` labels of the packages mounted, in mount order — the
    /// package list a `PackageContext` loads.
    packages: Vec<String>,
    /// Last `compile()` outputs `(synthetic path, body)`, indexed as local
    /// resources for snapshot base resolution.
    last_compiled: Vec<(PathBuf, Value)>,
}

thread_local! {
    static ENGINE: RefCell<Engine> = RefCell::new(Engine::default());
}

fn set_panic_hook() {
    #[cfg(target_family = "wasm")]
    console_error_panic_hook::set_once();
}

// ---------------------------------------------------------------------------
// JS-facing result shapes (serde -> JsValue via serde_wasm helpers we hand-roll
// with serde_json to avoid an extra dep: we return JSON strings the Worker
// JSON.parse()s, which is the simplest robust bindgen contract).
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

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

/// Mount a set of prebuilt package bundles as the in-memory package cache.
///
/// `bundles_json` is a JSON string: `[{ "label": "hl7.fhir.r4.core#4.0.1",
/// "files": { "<name>": "<base64 bytes>" , ... } }, ...]`. Each package's
/// `files` map is its already-inflated bundle entries (the browser fetched the
/// `.tgz`, inflated it via the `read_bundle` path or a JS gunzip, and base64'd
/// the bytes). Resources are indexed lazily on first fetch, so this is cheap.
///
/// Returns the number of packages mounted.
#[wasm_bindgen]
pub fn init(bundles_json: &str) -> Result<u32, JsError> {
    set_panic_hook();
    let parsed: Vec<BundleInput> = serde_json::from_str(bundles_json)
        .map_err(|e| JsError::new(&format!("init: bad bundles JSON: {e}")))?;
    let mut src = BundleSource::new();
    let mut labels = Vec::new();
    for pkg in &parsed {
        let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(pkg.files.len());
        for (name, b64) in &pkg.files {
            let bytes = base64_decode(b64)
                .map_err(|e| JsError::new(&format!("init: bad base64 for {name}: {e}")))?;
            entries.push((name.clone(), bytes));
        }
        src.mount_package(&pkg.label, entries);
        labels.push(pkg.label.clone());
    }
    let cache_root = src.cache_root().to_path_buf();
    ENGINE.with(|e| {
        let mut e = e.borrow_mut();
        e.bundle = Some(SharedBundle(Rc::new(src)));
        e.cache_root = cache_root;
        e.packages = labels;
        e.last_compiled.clear();
    });
    Ok(parsed.len() as u32)
}

#[derive(Deserialize)]
struct BundleInput {
    label: String,
    files: std::collections::BTreeMap<String, String>,
}

// ---------------------------------------------------------------------------
// compile
// ---------------------------------------------------------------------------

/// Compile a project in-memory. `files_json` is a JSON object mapping FSH file
/// paths to their text (e.g. `{ "input/fsh/Profiles.fsh": "..." }`); `config`
/// is the `sushi-config.yaml` text; `predefined_json` (may be `""`) is a JSON
/// object mapping `input/resources/**` paths to their JSON resource bodies.
///
/// Returns a JSON string `{ resources, diagnostics, timings }`. Resources carry
/// the byte-identical SUSHI output plus light metadata for the editor's views.
#[wasm_bindgen]
pub fn compile(files_json: &str, config: &str, predefined_json: &str) -> Result<String, JsError> {
    set_panic_hook();
    let (source, cache_root, _packages) = engine_source()?;

    // FSH files: object -> Vec sorted by path (matches the disk walk order).
    let files_map: std::collections::BTreeMap<String, String> = serde_json::from_str(files_json)
        .map_err(|e| JsError::new(&format!("compile: bad files JSON: {e}")))?;
    let fsh_files: Vec<(String, String)> = files_map.into_iter().collect();

    // Predefined resources: object path -> body. Sorted by path so
    // `PredefinedPackage::load_from` sees the disk-equivalent order.
    let predefined: Vec<(PathBuf, Value)> = if predefined_json.trim().is_empty() {
        Vec::new()
    } else {
        let m: std::collections::BTreeMap<String, Value> = serde_json::from_str(predefined_json)
            .map_err(|e| JsError::new(&format!("compile: bad predefined JSON: {e}")))?;
        m.into_iter().map(|(p, v)| (PathBuf::from(p), v)).collect()
    };

    let cache = cache_root.to_string_lossy().into_owned();
    let (compiled, diagnostics) =
        compiler::build_project_in_memory_with_diagnostics(config, &fsh_files, predefined, source, &cache)
            .map_err(|e| JsError::new(&format!("compile failed: {e:#}")))?;

    // Stash the compiled resources as local resources for snapshot resolution.
    let locals: Vec<(PathBuf, Value)> = compiled
        .iter()
        .map(|r| {
            (
                PathBuf::from(format!("/__compiled__/{}", r.filename)),
                r.body.clone(),
            )
        })
        .collect();
    ENGINE.with(|e| e.borrow_mut().last_compiled = locals);

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

    let out = CompileResult {
        resources,
        diagnostics,
        timings: Timings::default(),
    };
    serde_json::to_string(&out).map_err(|e| JsError::new(&format!("compile: serialize: {e}")))
}

// ---------------------------------------------------------------------------
// set_local_resources
// ---------------------------------------------------------------------------

/// Set the "local" StructureDefinitions the next `generate_snapshot` resolves
/// bases against — the in-memory equivalent of the CLI's `--local-dir`. `json`
/// is an object mapping a synthetic path to each SD's JSON body
/// (`{ "<name>.json": { ...SD... } }`). This is what the parity harness uses to
/// load a corpus IG's fixture set (the sibling profiles a rung's base resolves
/// to), and what an editor uses to snapshot against not-yet-recompiled siblings.
/// Replaces the local set from the last `compile()`.
#[wasm_bindgen]
pub fn set_local_resources(json: &str) -> Result<u32, JsError> {
    set_panic_hook();
    let map: std::collections::BTreeMap<String, Value> = serde_json::from_str(json)
        .map_err(|e| JsError::new(&format!("set_local_resources: bad JSON: {e}")))?;
    let locals: Vec<(PathBuf, Value)> = map
        .into_iter()
        .map(|(p, v)| (PathBuf::from(format!("/__local__/{p}")), v))
        .collect();
    let n = locals.len() as u32;
    ENGINE.with(|e| e.borrow_mut().last_compiled = locals);
    Ok(n)
}

// ---------------------------------------------------------------------------
// generate_snapshot
// ---------------------------------------------------------------------------

/// Generate a snapshot. `input` is either an inline StructureDefinition as a
/// JSON string, or a canonical profile URL (resolved against the last
/// `compile()`'s outputs, then the mounted packages). Returns a JSON string
/// `{ snapshot, messages }` where `snapshot` is the full SD with the generated
/// `snapshot.element`, R5-internal (the walk engine's native output).
#[wasm_bindgen]
pub fn generate_snapshot(input: &str) -> Result<String, JsError> {
    set_panic_hook();
    let ctx = build_context()?;

    // Inline SD if it parses as an object with resourceType StructureDefinition;
    // otherwise treat `input` as a URL/id/name and resolve it from local + pkgs.
    let derived: Value = match serde_json::from_str::<Value>(input.trim()) {
        Ok(v) if v.get("resourceType").and_then(|r| r.as_str()) == Some("StructureDefinition") => v,
        _ => {
            let query = input.trim();
            match ctx.fetch(query) {
                Some(rc) => (*rc).clone(),
                None => {
                    return serde_json::to_string(&SnapshotResult {
                        snapshot: None,
                        messages: vec![format!("no StructureDefinition found for '{query}'")],
                    })
                    .map_err(|e| JsError::new(&format!("serialize: {e}")));
                }
            }
        }
    };

    let out = match snapshot_gen::generate_snapshot(derived, &ctx, Default::default()) {
        Ok(v) => SnapshotResult {
            snapshot: Some(v),
            messages: Vec::new(),
        },
        Err(e) => SnapshotResult {
            snapshot: None,
            messages: vec![format!("{e:#}")],
        },
    };
    serde_json::to_string(&out).map_err(|e| JsError::new(&format!("serialize: {e}")))
}

// ---------------------------------------------------------------------------
// build_site_db — the M2 site-preview producer (docs/fhir-ig-editor-spec.md §7)
// ---------------------------------------------------------------------------

/// The JS input for [`build_site_db`]: the whole IG working set, in memory.
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
    /// project-relative path (e.g. `input/pagecontent/index.md`, `input/images/x.png`)
    /// -> base64 bytes. Text files may be base64'd UTF-8; images are raw bytes.
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

/// Build the site.db ROW MODEL from fully in-memory IG inputs and return it as a
/// JSON string the editor's JS row store consumes directly (M2 site preview).
///
/// Pipeline (all in the Worker, no filesystem, no C-sqlite):
///   S1/S2  compiler::build_project_in_memory_with_ig  -> conformance + the IG
///   S3     snapshot_gen::generate_snapshot (per SD)    -> snapshot-complete SDs
///   S5/S6  site_db::build_from_inputs                  -> the SiteDb row model
///
/// The returned JSON is `{ metadata, resources, concepts, valueSetCodes, pages,
/// menu, siteConfig, assets }` with SQLite/`core/db.ts` column casing (assets'
/// `Content` is base64). Byte/JSON-identical to the native disk site.db rows for
/// the same IG (minus BuildState timestamps) — asserted by the native
/// `inmem_vs_disk` parity test.
#[wasm_bindgen]
pub fn build_site_db(input_json: &str) -> Result<String, JsError> {
    set_panic_hook();
    let input: SiteDbInput = serde_json::from_str(input_json)
        .map_err(|e| JsError::new(&format!("build_site_db: bad input JSON: {e}")))?;

    let (source, cache_root, _packages) = engine_source()?;
    let cache = cache_root.to_string_lossy().into_owned();

    // ---- S1/S2 (+ IG export): compile in memory, producing the IG resource. ----
    let fsh_files: Vec<(String, String)> = input.fsh.into_iter().collect();
    let predefined: Vec<(PathBuf, Value)> = input
        .predefined
        .into_iter()
        .map(|(p, v)| (PathBuf::from(p), v))
        .collect();
    // The page-folder listing ig_export needs (folder -> filenames) is derived from
    // the site_files map: the disk path would scan input/{pagecontent,pages,
    // resource-docs}; we hand it the same names from the VFS.
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

    let (conformance, ig_resource, _diagnostics) =
        compiler::build_project_in_memory_with_ig(
            &input.config,
            &fsh_files,
            predefined,
            source,
            &cache,
            page_dir_listing,
        )
        .map_err(|e| JsError::new(&format!("build_site_db: compile failed: {e:#}")))?;

    // ---- S3: snapshot-complete each StructureDefinition against the compile. ----
    // Build the snapshot context EXACTLY as the native `site_db` pipeline does:
    // a `PackageContext` over ONLY the FHIR CORE package (r4/r5 core), plus the
    // just-compiled conformance SDs as locals so cross-profile bases resolve
    // (fact <- bleeding <- flow). Loading the whole mounted closure (uv.tools /
    // uv.extensions / terminology) here would pull extra type/extension profiles
    // into base resolution and inflate the snapshot vs the native oracle — the
    // native pipeline pins snapshotting to the single core package
    // (pipeline.rs `PackageContext::new(cache, [core_package])`), so we match it.
    let (source, cache_root, packages) = engine_source()?;
    let core_package = pick_core_package(&packages).ok_or_else(|| {
        JsError::new("build_site_db: no FHIR core package (hl7.fhir.r{4,5}.core) mounted")
    })?;
    let mut ctx = snapshot_gen::PackageContext::new_with(source, &cache_root, &[core_package])
        .map_err(|e| JsError::new(&format!("build_site_db: package context: {e:#}")))?;
    let locals: Vec<(PathBuf, Value)> = conformance
        .iter()
        .map(|r| (PathBuf::from(format!("/__compiled__/{}", r.filename)), r.body.clone()))
        .collect();
    ctx.load_local_resources(locals);

    let mut generated: Vec<Value> = Vec::new();
    for r in &conformance {
        if r.body.get("resourceType").and_then(Value::as_str) == Some("StructureDefinition") {
            let snap = snapshot_gen::generate_snapshot(r.body.clone(), &ctx, Default::default())
                .map_err(|e| {
                    JsError::new(&format!("build_site_db: snapshot {}: {e:#}", r.filename))
                })?;
            generated.push(snap);
        } else {
            generated.push(r.body.clone());
        }
    }
    if let Some(ig) = &ig_resource {
        generated.push(ig.body.clone());
    } else {
        return Err(JsError::new(
            "build_site_db: no ImplementationGuide produced (FSHOnly config or missing id)",
        ));
    }

    // Predefined `input/resources/**` bodies are the examples (S5 loadResources).
    // The compile already consumed them for the IG; re-collect them here as the
    // example resource set the row derivation orders after the conformance ones.
    let examples: Vec<Value> = collect_example_resources(&input.site_files);

    // ---- Site-content VFS for S6 (pagecontent/images/includes), keyed under /ig. ----
    let ig_root = PathBuf::from("/ig");
    let mut vfs: std::collections::BTreeMap<PathBuf, Vec<u8>> = std::collections::BTreeMap::new();
    for (path, b64) in &input.site_files {
        let bytes = base64_decode(b64)
            .map_err(|e| JsError::new(&format!("build_site_db: bad base64 for {path}: {e}")))?;
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
    .map_err(|e| JsError::new(&format!("build_site_db: assemble rows: {e:#}")))?;

    serde_json::to_string(&outcome.db)
        .map_err(|e| JsError::new(&format!("build_site_db: serialize rows: {e}")))
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

// ---------------------------------------------------------------------------
// version
// ---------------------------------------------------------------------------

/// Engine version + build commit, as a JSON string `{ version, commit }`.
#[wasm_bindgen]
pub fn version() -> String {
    let v = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("WASM_API_GIT_COMMIT").unwrap_or("unknown"),
        "engine": "rust_sushi + snapshot_gen (walk)",
    });
    v.to_string()
}

// ---------------------------------------------------------------------------
// internals
// ---------------------------------------------------------------------------

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

fn engine_source() -> Result<(SharedBundle, PathBuf, Vec<String>), JsError> {
    ENGINE.with(|e| {
        let e = e.borrow();
        let bundle = e
            .bundle
            .clone()
            .ok_or_else(|| JsError::new("engine not initialized: call init(bundles) first"))?;
        Ok((bundle, e.cache_root.clone(), e.packages.clone()))
    })
}

/// Build a fresh `PackageContext` over the mounted packages + the last compile's
/// local resources — the same shape `snapshot_gen --package ... --local-dir ...`
/// builds natively.
fn build_context() -> Result<snapshot_gen::PackageContext, JsError> {
    let (source, cache_root, packages) = engine_source()?;
    let mut ctx = snapshot_gen::PackageContext::new_with(source, &cache_root, &packages)
        .map_err(|e| JsError::new(&format!("package context: {e:#}")))?;
    let locals = ENGINE.with(|e| e.borrow().last_compiled.clone());
    ctx.load_local_resources(locals);
    Ok(ctx)
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
