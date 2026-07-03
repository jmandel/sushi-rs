//! Decision-isomorphic walk engine (REWORK-PLAN stage 3/4/5). Public entry:
//! `generate_snapshot`. Mirrors ProfileUtilities.generateSnapshot orchestration
//! (§1) + ProfilePathProcessor walk (§3) + finalize (§1.3). Everything is
//! R5-internal; R4 inputs and bases pass through convert.rs at load.

mod consts;
mod context;
mod contentref;
mod emit;
mod finalize;
mod frame;
pub(crate) mod ids;
mod loop_;
mod paths;
mod preprocess;
mod resolve;
mod simple;
mod slicing;
mod sliced;
mod sort;
mod trace;
mod types;
mod types_pred;
mod updatefromdef;

use anyhow::Context as _;
use serde_json::Value;
use std::rc::Rc;

use crate::{PackageContext, SnapshotOptions};
use context::{WalkConfig, WalkContext};
use frame::{SlicingParams, WalkCursor, WalkFrame};

/// Enable trace emission to `path` for the duration of the process. Called from
/// the CLI when `--trace`/`SNAPSHOT_TRACE` is set.
pub fn enable_trace(path: &str) -> std::io::Result<()> {
    trace::enable(path)
}

pub fn disable_trace() {
    trace::disable();
}

/// Top-level walk-engine entry (mirrors `PackageContext`-based legacy API).
pub fn generate_snapshot(
    derived: Value,
    pkg: &PackageContext,
    options: SnapshotOptions,
) -> anyhow::Result<Value> {
    generate_snapshot_opt_pin(derived, pkg, options, false)
}

/// Layer-A walk with an OPT-IN base-version-pinning flag (Layer B B1,
/// composition (a)). `pin_base_versions=false` is the ordinary Layer-A path,
/// byte-identical to before. `true` pins inherited base/dep SD snapshots so the
/// pins flow through inheritance (see `WalkContext::pin_base_versions`).
pub(crate) fn generate_snapshot_opt_pin(
    derived: Value,
    pkg: &PackageContext,
    options: SnapshotOptions,
    pin_base_versions: bool,
) -> anyhow::Result<Value> {
    let mut ctx = WalkContext {
        pkg,
        output: Vec::new(),
        output_ann: Vec::new(),
        diff: Rc::new(Vec::new()),
        diff_consumed: Vec::new(),
        diff_injected: Vec::new(),
        messages: Vec::new(),
        cfg: WalkConfig::default(),
        gen_cache: std::collections::HashMap::new(),
        gen_stack: Vec::new(),
        derived_url: String::new(),
        spec_url: String::new(),
        pin_base_versions,
    };
    // Convert R4 input to R5-internal.
    let derived = resolve::to_r5_internal(&derived)?;
    let out = generate_snapshot_with_opts(&mut ctx, derived, options.sort_differential, true)?;
    Ok(out)
}

/// Recursive generation entry used when a base/type SD lacks a snapshot
/// (resolve.rs). Uses a fresh sub-context so cursors/output/diff don't collide,
/// but shares the package + gen cache via re-resolution.
pub(crate) fn generate_snapshot_inner(
    parent: &mut WalkContext,
    sd: Value,
) -> anyhow::Result<Value> {
    let mut ctx = WalkContext {
        pkg: parent.pkg,
        output: Vec::new(),
        output_ann: Vec::new(),
        diff: Rc::new(Vec::new()),
        diff_consumed: Vec::new(),
        diff_injected: Vec::new(),
        messages: Vec::new(),
        cfg: WalkConfig::default(),
        gen_cache: std::mem::take(&mut parent.gen_cache),
        gen_stack: parent.gen_stack.clone(),
        derived_url: String::new(),
        spec_url: String::new(),
        pin_base_versions: parent.pin_base_versions,
    };
    // Nested generation (PPP:810 / PU:762): plain generateSnapshot — the
    // driver-level sortDifferential and bare-root prepend do NOT apply.
    let result = generate_snapshot_with_opts(&mut ctx, sd, false, false);
    parent.gen_cache = std::mem::take(&mut ctx.gen_cache);
    parent.messages.extend(ctx.messages.drain(..));
    result
}

fn generate_snapshot_with_opts(
    ctx: &mut WalkContext,
    mut derived: Value,
    sort: bool,
    top_level: bool,
) -> anyhow::Result<Value> {
    let url = derived
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    ctx.derived_url = url.clone();

    // Circular guard.
    if ctx.gen_stack.contains(&url) {
        anyhow::bail!("CIRCULAR_SNAPSHOT: {url}");
    }
    ctx.gen_stack.push(url.clone());

    let base_url = derived
        .get("baseDefinition")
        .and_then(Value::as_str)
        .context("StructureDefinition.baseDefinition is required")?
        .to_string();
    let base = resolve::resolve_with_snapshot(ctx, &base_url)?
        .with_context(|| format!("base not found: {base_url}"))?;
    let base_version = base.get("version").and_then(Value::as_str).map(str::to_string);

    // P6: fixTypeOfResourceId on the base (R4+ config). Base snapshots already
    // ship the fhir-type extension in R5; the R4 convert path handles it. No-op
    // here for R5 bases; documented gap for R4-core Resource.id if convert
    // didn't apply it (convert.rs owns this).

    // Preprocess (sortDifferential + slice-group push-down).
    let mut diff_elements: Vec<Value> = derived
        .get("differential")
        .and_then(|d| d.get("element"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if sort {
        let profile_name = format!("profile {url}");
        let base_for_sort = (*base).clone();
        let sort_errors =
            sort::sort_differential(ctx, &base_for_sort, &mut diff_elements, &profile_name)?;
        for e in sort_errors {
            ctx.add_message(context::Severity::Error, &profile_name, e);
        }
    }

    // Oracle driver root-prepend (SnapOracleR4:179-184): for the R4 path, if the
    // first diff element's path is absent or dotted (not the bare resource root),
    // prepend a bare root element `ElementDefinition().setPath(type or root)`.
    // The R5 oracle (SnapOracle) does not do this; gate on R4 input.
    let is_r4_input = top_level
        && derived
        .get("fhirVersion")
        .and_then(Value::as_str)
        .map(|v| v.starts_with('4'))
        .unwrap_or(false);
    if is_r4_input {
        let first_path = diff_elements
            .first()
            .and_then(|e| e.get("path").and_then(Value::as_str))
            .map(str::to_string);
        let need_root = match &first_path {
            None => true,
            Some(p) => p.contains('.'),
        };
        if need_root {
            let root_path = match &first_path {
                Some(p) => p.split('.').next().unwrap_or(p).to_string(),
                None => derived
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            };
            diff_elements.insert(0, serde_json::json!({ "path": root_path }));
        }
    }

    let mut base_elements: Vec<Value> = base
        .get("snapshot")
        .and_then(|s| s.get("element"))
        .and_then(Value::as_array)
        .cloned()
        .context("base StructureDefinition has no snapshot.element")?;

    // P18 trace: generateSnapshot.begin fires AFTER the diff clone but BEFORE
    // the preprocessor (PU:826-830) — diffElements counts pre-preprocess rows.
    if trace::active() {
        // Java passes the RESOLVED base SD (base.getUrl()), not the (possibly
        // versioned) baseDefinition query string.
        let resolved_base_url = base
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(&base_url)
            .to_string();
        trace::rec(
            "generateSnapshot",
            "generateSnapshot.begin",
            Some(&resolved_base_url),
            Some(&url),
            Some(serde_json::json!({
                "baseElements": base_elements.len(),
                "diffElements": diff_elements.len(),
                "derivation": derived.get("derivation").and_then(Value::as_str).unwrap_or(""),
            })),
        );
    }

    let derived_versioned_url = match derived.get("version").and_then(Value::as_str) {
        Some(v) if !v.is_empty() => format!("{url}|{v}"),
        _ => url.clone(),
    };
    let injected = preprocess::process(ctx, &mut diff_elements, &derived_versioned_url)?;

    // P6 fixTypeOfResourceId (PU:1305): for R4+ resource bases, rewrite every
    // element whose base.path == "Resource.id" to System.String with a fhir-type
    // extension of "id".
    let base_is_resource = base.get("kind").and_then(Value::as_str) == Some("resource");
    let base_is_r4plus = base
        .get("fhirVersion")
        .and_then(Value::as_str)
        .map(|v| v.starts_with('4') || v.starts_with('5'))
        .unwrap_or(true);
    if base_is_resource && base_is_r4plus {
        fix_type_of_resource_id(&mut base_elements);
    }

    ctx.diff_consumed = vec![false; diff_elements.len()];
    ctx.diff_injected = injected;
    ctx.diff = Rc::new(diff_elements);
    ctx.output = Vec::new();
    ctx.output_ann = Vec::new();

    // The walk.
    let base_source_url = base.get("url").and_then(Value::as_str).unwrap_or(&base_url).to_string();
    let mut cur = WalkCursor {
        base_source_url: base_source_url.clone(),
        base: Rc::new(base_elements.clone()),
        base_cursor: 0,
        diff_cursor: 0,
        context_name: base_source_url.clone(),
        result_path_base: None,
    };
    let diff_limit = if ctx.diff.is_empty() {
        -1
    } else {
        ctx.diff.len() as isize - 1
    };
    let web_url = derived
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string);
    let frame = WalkFrame {
        base_limit: base_elements.len().saturating_sub(1),
        diff_limit,
        url: url.clone(),
        web_url,
        profile_name: derived
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        context_path_source: None,
        context_path_target: None,
        trim_differential: false,
        redirector: Vec::new(),
        source_sd_url: base_source_url.clone(),
        spec_url: spec_url_for(&base),
        slicing: SlicingParams::default(),
    };
    ctx.spec_url = frame.spec_url.clone();

    loop_::process_paths(ctx, &mut cur, &frame, None)?;

    if trace::active() {
        trace::rec(
            "generateSnapshot",
            "generateSnapshot.walkComplete",
            None,
            Some(&url),
            Some(serde_json::json!({ "snapshotElements": ctx.output.len() })),
        );
    }

    finalize::finalize(ctx, &mut derived, base_version.as_deref())?;

    ctx.gen_stack.pop();
    Ok(derived)
}

const EXT_FHIR_TYPE: &str = "http://hl7.org/fhir/StructureDefinition/structuredefinition-fhir-type";

/// PU:1312 fixTypeOfResourceId — rewrite `Resource.id`-based elements' type.
fn fix_type_of_resource_id(elements: &mut [Value]) {
    for ed in elements.iter_mut() {
        let is_resource_id = ed
            .get("base")
            .and_then(|b| b.get("path"))
            .and_then(Value::as_str)
            == Some("Resource.id");
        if !is_resource_id {
            continue;
        }
        if let Some(types) = ed.get_mut("type").and_then(Value::as_array_mut) {
            for tr in types {
                if let Some(obj) = tr.as_object_mut() {
                    obj.insert(
                        "code".to_string(),
                        Value::String("http://hl7.org/fhirpath/System.String".to_string()),
                    );
                    // remove existing fhir-type ext, add "id".
                    let mut exts: Vec<Value> = obj
                        .get("extension")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    exts.retain(|e| e.get("url").and_then(Value::as_str) != Some(EXT_FHIR_TYPE));
                    exts.push(serde_json::json!({ "url": EXT_FHIR_TYPE, "valueUrl": "id" }));
                    obj.insert("extension".to_string(), Value::Array(exts));
                }
            }
        }
    }
}

/// context.getSpecUrl() equivalent (SimpleWorkerContext:964 →
/// VersionUtilities.getSpecUrl + "/"): 4.0→R4, 4.3→R4B, 5.0→R5.
fn spec_url_for(base: &Value) -> String {
    match base.get("fhirVersion").and_then(Value::as_str) {
        Some(v) if v.starts_with("4.0") => "http://hl7.org/fhir/R4/".to_string(),
        Some(v) if v.starts_with("4.3") => "http://hl7.org/fhir/R4B/".to_string(),
        Some(v) if v.starts_with("3.0") => "http://hl7.org/fhir/STU3/".to_string(),
        Some(v) if v.starts_with('5') => "http://hl7.org/fhir/R5/".to_string(),
        _ => "http://hl7.org/fhir/R5/".to_string(),
    }
}
