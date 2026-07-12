//! `fig` — the unified FHIR IG CLI. ONE binary; subcommands map onto the SAME
//! target-neutral SiteEngine core used by the WASM Session.
//!
//! Subcommands are transport adapters: parse arguments, call a typed library
//! operation, and serialize its result. They do not assemble renderer state.
//!
//! `--json` on every subcommand emits the shared `api_envelope` envelope,
//! schema-identical to the Session's.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use api_envelope::{envelope, API_VERSION};
use serde_json::{json, Value};

#[cfg(not(target_family = "wasm"))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let json = args.iter().any(|a| a == "--json");
    let sub = args.get(1).map(String::as_str);
    let op = sub.unwrap_or("");

    let result: Result<Value> = match sub {
        Some("build") => cmd_build(&args),
        Some("snapshot") => cmd_snapshot(&args),
        Some("resolve") => cmd_resolve(&args),
        Some("packages") => cmd_packages(&args),
        Some("expand") => cmd_expand(&args),
        Some("prepare") => cmd_prepare(&args),
        Some("outputs") => cmd_outputs(&args),
        Some("render") => cmd_render(&args),
        Some("finalize") => cmd_finalize(&args),
        Some("version") | Some("--version") => {
            let v = fig::version_payload();
            if !json {
                println!(
                    "fig {} (engine: {}, apiVersion {})",
                    v["version"].as_str().unwrap_or("?"),
                    v["engine"].as_str().unwrap_or("?"),
                    API_VERSION,
                );
            }
            Ok(v)
        }
        Some("-h") | Some("--help") | None => {
            print_usage();
            return;
        }
        Some(other) => Err(anyhow::anyhow!("unknown subcommand: {other}")),
    };

    match result {
        Ok(payload) => {
            if json {
                println!("{}", envelope(op, Ok(payload)));
            } else if let Some(text) = payload.get("__human").and_then(Value::as_str) {
                // A subcommand may stash a human-formatted string under __human;
                // otherwise the payload is already printed by the command.
                print!("{text}");
            }
        }
        Err(e) => {
            if json {
                println!("{}", envelope(op, Err(format!("{e:#}"))));
            } else {
                eprintln!("fig {op}: {e:#}");
                std::process::exit(1);
            }
        }
    }
}

// ===========================================================================
// build — FSH -> resources (rust_sushi build)
// ===========================================================================
fn cmd_build(args: &[String]) -> Result<Value> {
    let ig = positional(args, 2).context("usage: fig build <ig-dir> [-o <out>] [--cache <dir>]")?;
    let out = opt(args, "-o")
        .or_else(|| opt(args, "--out"))
        .unwrap_or("fsh-generated");
    let human = !has(args, "--json");
    match opt(args, "--cache") {
        Some(cache) => compiler::build_project_with_cache(ig, out, cache)?,
        None => compiler::build_project(ig, out)?,
    }
    if human {
        println!("fig build: {ig} -> {out}");
    }
    Ok(json!({ "ig": ig, "out": out }))
}

// ===========================================================================
// snapshot — walk-engine snapshots (batch or single) [snapshot_gen]
// ===========================================================================
fn cmd_snapshot(args: &[String]) -> Result<Value> {
    // fig snapshot <sd.json> [--package <pkg#ver> ...] [--cache <dir>] [--local-dir <d> ...]
    let input = positional(args, 2)
        .context("usage: fig snapshot <sd.json> [--package pkg#ver ...] [--cache <dir>]")?;
    let mut packages: Vec<String> = collect(args, "--package");
    packages.extend(collect(args, "-p"));
    if packages.is_empty() {
        packages.push("hl7.fhir.r5.core#5.0.0".to_string());
    }
    let cache = opt(args, "--cache")
        .map(str::to_string)
        .or_else(|| std::env::var("FHIR_CACHE").ok())
        .unwrap_or_else(|| "temp/fhir-home/.fhir/packages".to_string());
    let mut ctx = snapshot_gen::PackageContext::new(&cache, &packages)?;
    for d in collect(args, "--local-dir") {
        ctx.load_local_dir(d)?;
    }
    let source = std::fs::read_to_string(input).with_context(|| format!("read {input}"))?;
    let derived: Value = serde_json::from_str(&source)?;
    let out = snapshot_gen::generate_snapshot(derived, &ctx, Default::default())?;
    if has(args, "--json") {
        Ok(out)
    } else {
        print!("{}", json_emit::to_fhir_json_string(&out));
        Ok(out)
    }
}

// ===========================================================================
// resolve — dependency closure (compile set + context closure) [package_store]
// ===========================================================================
fn cmd_resolve(args: &[String]) -> Result<Value> {
    let cache = opt(args, "--cache")
        .context("usage: fig resolve --cache <dir> (--root <id#ver> | --project <ig>)")?;
    let cache_path = Path::new(cache);
    let source = package_store::DiskSource;
    let index = package_store::version_index_from_cache(&source, cache_path);
    let human = !has(args, "--json");
    if let Some(root) = opt(args, "--root") {
        let closure =
            package_store::context_closure_for_root(&source, cache_path, root, Some(&index))?;
        let list: Vec<String> = closure
            .iter()
            .map(|r| format!("{}#{}", r.package_id, r.version))
            .collect();
        if human {
            for l in &list {
                println!("{l}");
            }
        }
        Ok(json!({ "root": root, "closure": list }))
    } else if let Some(ig) = opt(args, "--project") {
        let cfg = std::fs::read_to_string(Path::new(ig).join("sushi-config.yaml"))?;
        let step = package_store::resolve_project(&cfg, &source, cache_path, Some(&index))?;
        let v: Value = serde_json::from_str(&step.to_json())?;
        if human {
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Ok(v)
    } else {
        bail!("resolve needs --root <id#ver> or --project <ig-dir>");
    }
}

// ===========================================================================
// packages fetch|bundle — acquisition / CDN bundle production
// ===========================================================================
fn cmd_packages(args: &[String]) -> Result<Value> {
    match positional(args, 2) {
        Some("prepare") => {
            let request = package_acquisition::PreparedPackageSetRequest::parse_cli(&args[3..])?;
            let manifest = request.execute()?;
            let value = serde_json::to_value(&manifest)?;
            if !has(args, "--json") {
                println!("{}", serde_json::to_string_pretty(&value)?);
            }
            Ok(value)
        }
        Some("bundle") => {
            let cache = opt(args, "--cache").context("packages bundle needs --cache <dir>")?;
            let out = opt(args, "-o")
                .or_else(|| opt(args, "--out"))
                .context("packages bundle needs --out <dir>")?;
            let labels = package_labels(args, 3)?;
            if labels.is_empty() {
                bail!("packages bundle needs at least one <id#version>");
            }
            let manifest =
                package_acquisition::build_bundle_set(Path::new(cache), &labels, Path::new(out))?;
            let bytes = manifest.to_bytes();
            if !has(args, "--json") {
                print!("{}", String::from_utf8_lossy(&bytes));
            }
            Ok(serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| json!({ "out": out, "packages": labels })))
        }
        Some("fetch") => {
            use package_acquisition::{default_registries, Coordinate, PackageCas};
            let coord = positional(args, 3)
                .context("packages fetch <id#ver> [--cas <dir>] [--registry <url>]")?;
            let coord = Coordinate::parse(coord)?;
            let cas = PackageCas::new(
                opt(args, "--cas").map_or(PackageCas::default_root()?, PathBuf::from),
            );
            let registries = opt(args, "--registry")
                .map(|r| vec![r.to_string()])
                .unwrap_or_else(default_registries);
            let pkg = cas.acquire_remote(&coord, &registries)?;
            let v = serde_json::to_value(&pkg)?;
            if !has(args, "--json") {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            Ok(v)
        }
        _ => bail!(
            "usage: fig packages <fetch <id#ver> | bundle|prepare --cache <dir> --out <dir> <id#ver>...>"
        ),
    }
}

// ===========================================================================
// expand — tier-1 enumerable ValueSet expansion [compiler::terminology]
// ===========================================================================
fn cmd_expand(args: &[String]) -> Result<Value> {
    use compiler::terminology::{expand_enumerable, MapResolver};
    let vs_path =
        positional(args, 2).context("usage: fig expand <valueset.json> [--resources <r.json>]")?;
    let vs: Value = serde_json::from_str(&std::fs::read_to_string(vs_path)?)?;
    let mut resolver = MapResolver::new();
    if let Some(rp) = opt(args, "--resources") {
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(rp)?)?;
        match parsed {
            Value::Array(items) => items.into_iter().for_each(|r| {
                resolver.insert(r);
            }),
            Value::Object(map) => map.into_iter().for_each(|(_k, r)| {
                resolver.insert(r);
            }),
            _ => bail!("--resources must be a JSON array or object"),
        }
    }
    let payload = match expand_enumerable(&vs, &resolver) {
        Ok(exp) => {
            json!({ "ok": true, "expansion": exp.to_expansion_json(), "copyright": exp.copyright() })
        }
        Err(ne) => {
            json!({ "ok": false, "notEnumerable": { "reason": ne.reason, "display": ne.to_string() } })
        }
    };
    if !has(args, "--json") {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    }
    Ok(payload)
}

// ===========================================================================
// prepare — exact native compile -> ClosedSiteBuild + filesystem CAS
// ===========================================================================
fn cmd_prepare(args: &[String]) -> Result<Value> {
    let ig = positional(args, 2).context(
        "usage: fig prepare <ig-dir> --target <cycle-site/v2|publisher-site/v1> --cache <dir> --out <new-dir> --build-date <epoch|RFC3339> [--template <id#version>]",
    )?;
    let target = opt(args, "--target").context("--target is required")?;
    let cache = opt(args, "--cache").context("--cache <dir> is required")?;
    let out = opt(args, "--out")
        .or_else(|| opt(args, "-o"))
        .context("--out <new-bundle-dir> is required")?;
    let build_epoch_secs = resolve_build_epoch(opt(args, "--build-date"))?;
    let outcome = fig::prepare::prepare(&fig::prepare::PrepareConfig {
        ig_dir: PathBuf::from(ig),
        cache_dir: PathBuf::from(cache),
        out_dir: PathBuf::from(out),
        target: target.to_string(),
        template_coordinate: opt(args, "--template").map(str::to_string),
        active_tables: has(args, "--active-tables"),
        run_uuid: opt(args, "--run-uuid").map(str::to_string),
        build_epoch_secs,
    })?;
    if !has(args, "--json") {
        eprintln!(
            "fig prepare: {} sources + {} packages -> {} objects at {} ({})",
            outcome.sources, outcome.packages, outcome.objects, outcome.out, outcome.build_id,
        );
    }
    Ok(serde_json::to_value(outcome)?)
}

// ===========================================================================
// outputs/render/finalize — native transport over the canonical SiteEngine
// ===========================================================================
fn cmd_outputs(args: &[String]) -> Result<Value> {
    let bundle = positional(args, 2).context("usage: fig outputs <closed-bundle>")?;
    if let Some(cache) = opt(args, "--cache") {
        let total_started = Instant::now();
        let verify_started = Instant::now();
        let closed = fig::output_cache::admit(
            Path::new(bundle),
            Path::new(cache),
            opt(args, "--into").map(Path::new),
        )?;
        let verify_input_ms = millis(verify_started.elapsed());
        let (renderer, output_schema, options) = output_cache_derivation(args)?;
        let lookup_started = Instant::now();
        let loaded = fig::output_cache::load(
            &closed,
            Path::new(cache),
            &renderer,
            &output_schema,
            &options,
        )?;
        let lookup_ms = millis(lookup_started.elapsed());
        let materialize_started = Instant::now();
        if let (Some(output), Some(destination)) = (&loaded.output, opt(args, "--into")) {
            fig::output_cache::materialize(output, Path::new(cache), Path::new(destination))?;
        }
        return Ok(json!({
            "cacheHit": loaded.output.is_some(),
            "cacheKey": loaded.cache_key,
            "outputId": loaded.output.as_ref().map(|output| output.output_id()),
            "files": loaded.output.as_ref().map_or(0, |output| output.files().len()),
            "out": loaded.output.as_ref().and_then(|_| opt(args, "--into")),
            "timings": {
                "verifyInputMs": verify_input_ms,
                "lookupMs": lookup_ms,
                "materializeMs": millis(materialize_started.elapsed()),
                "totalMs": millis(total_started.elapsed()),
            },
        }));
    }
    let catalog = fig::site::outputs(Path::new(bundle))?;
    let payload = serde_json::to_value(&catalog)?;
    if !has(args, "--json") {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    }
    Ok(payload)
}

fn cmd_render(args: &[String]) -> Result<Value> {
    let bundle =
        positional(args, 2).context("usage: fig render <closed-bundle> <path> [-o <file>]")?;
    let path = positional(args, 3).context("render needs one path declared by fig outputs")?;
    let outcome = fig::site::render(Path::new(bundle), path)?;
    if let Some(output) = opt(args, "-o").or_else(|| opt(args, "--out")) {
        std::fs::write(output, &outcome.bytes)
            .with_context(|| format!("write rendered output {output}"))?;
    } else if !has(args, "--json") {
        std::io::stdout().write_all(&outcome.bytes)?;
    }
    Ok(serde_json::to_value(outcome)?)
}

fn cmd_finalize(args: &[String]) -> Result<Value> {
    let bundle = positional(args, 2).context(
        "usage: fig finalize <closed-bundle> (-o <new-site-dir> | --site <staging> --external-plan <file|->)",
    )?;
    if let Some(site) = opt(args, "--site") {
        let plan_path = opt(args, "--external-plan")
            .context("external finalize needs --external-plan <file|->")?;
        let mut bytes = Vec::new();
        if plan_path == "-" {
            std::io::stdin().read_to_end(&mut bytes)?;
        } else {
            bytes = std::fs::read(plan_path)
                .with_context(|| format!("read external finalize plan {plan_path}"))?;
        }
        let plan: fig::site::ExternalFinalizePlan = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse external finalize plan {plan_path}"))?;
        let outcome = fig::site::finalize(
            Path::new(bundle),
            fig::site::FinalizeRequest::External {
                site: Path::new(site),
                plan,
                cache_root: opt(args, "--cache").map(Path::new),
            },
        )?;
        if !has(args, "--json") {
            eprintln!(
                "fig finalize: authenticated {} external files ({} bytes) at {} ({})",
                outcome.files, outcome.bytes, outcome.out, outcome.output_id
            );
        }
        return Ok(serde_json::to_value(outcome)?);
    }
    let output = opt(args, "-o")
        .or_else(|| opt(args, "--out"))
        .context("finalize needs -o <new-site-dir>")?;
    let outcome = fig::site::finalize(
        Path::new(bundle),
        fig::site::FinalizeRequest::Publisher {
            destination: Path::new(output),
        },
    )?;
    if !has(args, "--json") {
        eprintln!(
            "fig finalize: {} files ({} bytes) -> {} ({})",
            outcome.files, outcome.bytes, outcome.out, outcome.output_id
        );
    }
    Ok(serde_json::to_value(outcome)?)
}

fn output_cache_derivation(
    args: &[String],
) -> Result<(
    site_build::RendererImplementation,
    String,
    BTreeMap<String, String>,
)> {
    let renderer_id =
        opt(args, "--renderer-id").context("outputs cache probe needs --renderer-id")?;
    let renderer_version =
        opt(args, "--renderer-version").context("outputs cache probe needs --renderer-version")?;
    let recipe = opt(args, "--recipe-sha256")
        .context("outputs cache probe needs --recipe-sha256 <64 lowercase hex>")?;
    let output_schema =
        opt(args, "--output-schema").context("outputs cache probe needs --output-schema")?;
    let mut options = BTreeMap::new();
    for option in collect(args, "--option") {
        let (key, value) = option
            .split_once('=')
            .with_context(|| format!("outputs option must be key=value, got {option}"))?;
        if options.insert(key.to_string(), value.to_string()).is_some() {
            bail!("outputs option repeats key {key}");
        }
    }
    Ok((
        site_build::RendererImplementation {
            id: renderer_id.to_string(),
            version: renderer_version.to_string(),
            recipe_sha256: site_build::Sha256Digest::parse(recipe)?,
        },
        output_schema.to_string(),
        options,
    ))
}

fn millis(duration: std::time::Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn positional<'a>(args: &'a [String], i: usize) -> Option<&'a str> {
    args.get(i)
        .map(String::as_str)
        .filter(|s| !s.starts_with('-'))
}
fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}
fn collect(args: &[String], name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (i, a) in args.iter().enumerate() {
        if a == name {
            if let Some(v) = args.get(i + 1) {
                out.push(v.clone());
            }
        }
    }
    out
}
fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn package_labels(args: &[String], start: usize) -> Result<Vec<String>> {
    let mut labels = Vec::new();
    let mut index = start;
    while index < args.len() {
        match args[index].as_str() {
            "--cache" | "--out" | "-o" => {
                index += 2;
            }
            "--json" => index += 1,
            value if value.starts_with('-') => {
                bail!("packages bundle: unknown option {value}")
            }
            value => {
                if value.contains('#') {
                    labels.push(value.to_string());
                }
                index += 1;
            }
        }
    }
    Ok(labels)
}

fn resolve_build_epoch(arg: Option<&str>) -> Result<i64> {
    let raw = match arg {
        Some(v) => v.to_string(),
        None => std::env::var("SOURCE_DATE_EPOCH").context(
            "no build timestamp: pass --build-date <epoch|RFC3339> or set SOURCE_DATE_EPOCH",
        )?,
    };
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<i64>() {
        return Ok(secs);
    }
    let b = raw.as_bytes();
    if b.len() >= 19 && b[4] == b'-' && b[10] == b'T' {
        let y: i64 = raw[0..4].parse()?;
        let mo: i64 = raw[5..7].parse()?;
        let d: i64 = raw[8..10].parse()?;
        let h: i64 = raw[11..13].parse()?;
        let mi: i64 = raw[14..16].parse()?;
        let s: i64 = raw[17..19].parse()?;
        let y = if mo <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        return Ok(days * 86_400 + h * 3600 + mi * 60 + s);
    }
    bail!("unrecognized --build-date '{raw}': want a unix epoch or YYYY-MM-DDThh:mm:ssZ")
}

fn print_usage() {
    eprintln!(
        "fig {} (apiVersion {}) — the unified FHIR IG CLI\n\
         \n\
         USAGE: fig <command> [args] [--json]\n\
         \n\
         COMMANDS:\n\
         \x20 build <ig-dir> [-o <out>] [--cache <dir>]        FSH -> resources\n\
         \x20 snapshot <sd.json> [--package p#v ...] [--cache] Walk-engine snapshot\n\
         \x20 resolve --cache <dir> (--root i#v | --project d) Dependency closure\n\
         \x20 packages fetch <i#v> | bundle|prepare --cache -o <dir> Package artifacts\n\
         \x20 expand <vs.json> [--resources <r.json>]          Tier-1 VS expansion\n\
         \x20 prepare <ig> --target <cycle-site/v2|publisher-site/v1>\n\
         \x20         --cache <d> --out <new> --build-date <time>\n\
         \x20         [--template <id#version>]                Closed SiteBuild bundle\n\
         \x20 outputs <bundle>                                 Output catalog/cache probe\n\
         \x20 render <bundle> <path> [-o <file>]               Render one declared output\n\
         \x20 finalize <bundle> -o <new-site-dir>              Canonical Publisher site\n\
         \x20 finalize <bundle> --site <staging>               Canonical external receipt\n\
         \x20         --external-plan <file|-> [--cache <d>]\n\
         \x20 version                                          Engine + pins\n\
         \n\
         Add --json to any command for the apiVersion result envelope.",
        env!("CARGO_PKG_VERSION"),
        API_VERSION,
    );
}
