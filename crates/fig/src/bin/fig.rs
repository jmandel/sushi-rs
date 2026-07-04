//! `fig` — the unified FHIR IG CLI. ONE binary; subcommands map onto the SAME
//! engine core the wasm Session exposes (docs/unified-cli-plan.md).
//!
//! IRON RULE: subcommands contain NO logic — each is arg-parse → engine-core
//! call → output. Any composition the engine lacks lives in `fig::engine` (a
//! native engine module the Session can grow later), not here.
//!
//! `--json` on every subcommand emits the shared `api_envelope` envelope,
//! schema-identical to the Session's.

use std::path::{Path, PathBuf};

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
        Some("sitedb") => cmd_sitedb(&args),
        Some("fragment") => cmd_fragment(&args),
        Some("fragments") => cmd_fragments(&args),
        Some("render") => cmd_render(&args),
        Some("watch") => cmd_watch(&args),
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
    let out = opt(args, "-o").or_else(|| opt(args, "--out")).unwrap_or("fsh-generated");
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
    let input = positional(args, 2).context("usage: fig snapshot <sd.json> [--package pkg#ver ...] [--cache <dir>]")?;
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
    let cache = opt(args, "--cache").context("usage: fig resolve --cache <dir> (--root <id#ver> | --project <ig>)")?;
    let cache_path = Path::new(cache);
    let source = package_store::DiskSource;
    let index = package_store::version_index_from_cache(&source, cache_path);
    let human = !has(args, "--json");
    if let Some(root) = opt(args, "--root") {
        let closure = package_store::context_closure_for_root(&source, cache_path, root, Some(&index))?;
        let list: Vec<String> = closure.iter().map(|r| format!("{}#{}", r.package_id, r.version)).collect();
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
        Some("bundle") => {
            // `--template <id#ver>` emits the editor's warm-start artifact from the
            // LOADER's materialized template tree (task #39: the packed-bundle the
            // editor consumes becomes an artifact the loader emits — same bytes the
            // parity gate proves). Otherwise the regular package-bundle path.
            if let Some(coord) = opt(args, "--template") {
                return cmd_bundle_template(args, coord);
            }
            let cache = opt(args, "--cache").context("packages bundle needs --cache <dir>")?;
            let out = opt(args, "-o").or_else(|| opt(args, "--out")).context("packages bundle needs --out <dir>")?;
            let labels: Vec<String> = args.iter().skip(3).filter(|a| !a.starts_with('-') && a.contains('#')).cloned().collect();
            if labels.is_empty() {
                bail!("packages bundle needs at least one <id#version> (or --template <id#ver>)");
            }
            let manifest = package_acquisition::build_bundle_set(Path::new(cache), &labels, Path::new(out))?;
            let bytes = manifest.to_bytes();
            if !has(args, "--json") {
                print!("{}", String::from_utf8_lossy(&bytes));
            }
            Ok(serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({ "out": out, "packages": labels })))
        }
        Some("fetch") => {
            use package_acquisition::{default_registries, Coordinate, PackageCas};
            let coord = positional(args, 3).context("packages fetch <id#ver> [--cas <dir>] [--registry <url>]")?;
            let coord = Coordinate::parse(coord)?;
            let cas = PackageCas::new(
                opt(args, "--cas").map_or(PackageCas::default_root()?, PathBuf::from),
            );
            let registries = opt(args, "--registry").map(|r| vec![r.to_string()]).unwrap_or_else(default_registries);
            let pkg = cas.acquire_remote(&coord, &registries)?;
            let v = serde_json::to_value(&pkg)?;
            if !has(args, "--json") {
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            Ok(v)
        }
        _ => bail!("usage: fig packages <fetch <id#ver> | bundle --cache <dir> --out <dir> <id#ver>...>"),
    }
}

/// `fig packages bundle --template <id#ver>` — materialize the template chain via
/// the LOADER and emit it as the editor's warm-start artifact: a `mountSite`-
/// compatible files-JSON (`{ "<rel>": "<text>" | {"b64":"<bytes>"} }`), plus a
/// small manifest. This is the SAME materialized bytes the parity gate proves, so
/// the editor's warm-start template tree is loader-produced (task #39). Text files
/// go verbatim; binary assets are base64'd — exactly the shape `mountSite` /
/// `mountTemplate` parse.
fn cmd_bundle_template(args: &[String], coord: &str) -> Result<Value> {
    let out = opt(args, "-o").or_else(|| opt(args, "--out"))
        .context("packages bundle --template needs -o <file.json>")?;
    let cache = opt(args, "--template-cache")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("fig-template-cache"));
    let cas = package_acquisition::PackageCas::new(package_acquisition::PackageCas::default_root()?);
    let registries = package_acquisition::default_registries();
    let offline = has(args, "--offline");
    let tree = fig::template::acquire_and_materialize(coord, &cache, &cas, &registries, offline)?;

    // Build the mountSite files-JSON: UTF-8 text verbatim, else {"b64":...}.
    let mut files = serde_json::Map::new();
    let mut binary = 0usize;
    for (rel, bytes) in tree.files() {
        let v = match std::str::from_utf8(bytes) {
            Ok(text) => json!(text),
            Err(_) => {
                binary += 1;
                json!({ "b64": fig::template::b64_encode(bytes) })
            }
        };
        files.insert(rel.clone(), v);
    }
    let doc = json!({
        "template": coord,
        "files": Value::Object(files),
        "fileCount": tree.len(),
        "binaryCount": binary,
    });
    std::fs::write(out, serde_json::to_vec(&doc)?)
        .with_context(|| format!("write {out}"))?;
    if !has(args, "--json") {
        eprintln!("fig packages bundle --template {coord}: {} files ({binary} binary) -> {out}", tree.len());
    }
    Ok(json!({ "out": out, "template": coord, "fileCount": tree.len(), "binaryCount": binary }))
}

// ===========================================================================
// expand — tier-1 enumerable ValueSet expansion [compiler::terminology]
// ===========================================================================
fn cmd_expand(args: &[String]) -> Result<Value> {
    use compiler::terminology::{expand_enumerable, MapResolver};
    let vs_path = positional(args, 2).context("usage: fig expand <valueset.json> [--resources <r.json>]")?;
    let vs: Value = serde_json::from_str(&std::fs::read_to_string(vs_path)?)?;
    let mut resolver = MapResolver::new();
    if let Some(rp) = opt(args, "--resources") {
        let parsed: Value = serde_json::from_str(&std::fs::read_to_string(rp)?)?;
        match parsed {
            Value::Array(items) => items.into_iter().for_each(|r| { resolver.insert(r); }),
            Value::Object(map) => map.into_iter().for_each(|(_k, r)| { resolver.insert(r); }),
            _ => bail!("--resources must be a JSON array or object"),
        }
    }
    let payload = match expand_enumerable(&vs, &resolver) {
        Ok(exp) => json!({ "ok": true, "expansion": exp.to_expansion_json(), "copyright": exp.copyright() }),
        Err(ne) => json!({ "ok": false, "notEnumerable": { "reason": ne.reason, "display": ne.to_string() } }),
    };
    if !has(args, "--json") {
        println!("{}", serde_json::to_string_pretty(&payload)?);
    }
    Ok(payload)
}

// ===========================================================================
// sitedb — S1-S7 producer (site_db build)
// ===========================================================================
fn cmd_sitedb(args: &[String]) -> Result<Value> {
    let ig = positional(args, 2).context("usage: fig sitedb <ig-dir> --sushi-out <dir> --cache <dir> -o <site.db>")?;
    let sushi_out = opt(args, "--sushi-out").context("--sushi-out <dir> is required")?;
    let cache = opt(args, "--cache").context("--cache <dir> is required")?;
    let out_db = opt(args, "-o").or_else(|| opt(args, "--out")).context("-o <site.db> is required")?;
    let build_epoch = resolve_build_epoch(opt(args, "--build-date"))?;
    let config = site_db::BuildConfig {
        ig_dir: PathBuf::from(ig),
        sushi_out: PathBuf::from(sushi_out),
        cache_dir: PathBuf::from(cache),
        out_db: PathBuf::from(out_db),
        build_epoch_secs: build_epoch,
        branch: opt(args, "--branch").map(str::to_string),
        revision: opt(args, "--revision").map(str::to_string),
        run_sushi: !has(args, "--no-sushi"),
        core_package: opt(args, "--core").unwrap_or("hl7.fhir.r4.core#4.0.1").to_string(),
        layer_b: snapshot_gen::LayerBOptions::default(),
    };
    let report = site_db::build_and_write(&config)?;
    if !has(args, "--json") {
        eprintln!(
            "fig sitedb: {} nodes ({} clean, {} dirty){}",
            report.ledger.nodes.len(), report.clean.len(), report.dirty.len(),
            if report.no_op { " — NO-OP" } else { "" },
        );
        println!("{out_db}");
    }
    Ok(json!({
        "out": out_db,
        "nodes": report.ledger.nodes.len(),
        "clean": report.clean.len(),
        "dirty": report.dirty.len(),
        "noOp": report.no_op,
    }))
}

// ===========================================================================
// fragment — render ONE fragment (the CLI face of first-include-miss)
// ===========================================================================
fn cmd_fragment(args: &[String]) -> Result<Value> {
    // fig fragment <build-dir> <ref> <kind> [--active-tables] [--run-uuid <u>]
    let build = positional(args, 2).context("usage: fig fragment <build-dir> <ref> <kind>")?;
    let ref_ = positional(args, 3).context("fragment needs <ref>")?;
    let kind = positional(args, 4).context("fragment needs <kind>")?;
    let root = fig::engine::RenderRoot::detect(Path::new(build))?;
    let opts = render_opts(args);
    let body = fig::engine::render_one_fragment(&root, &opts, ref_, kind)?;
    if !has(args, "--json") {
        print!("{body}");
    }
    Ok(json!({ "ref": ref_, "kind": kind, "html": body }))
}

// ===========================================================================
// fragments -o — the files escape hatch (§3b)
// ===========================================================================
fn cmd_fragments(args: &[String]) -> Result<Value> {
    // fig fragments <build-dir> -o <dir> [--kinds k1,k2,...] [--ref R]
    let build = positional(args, 2).context("usage: fig fragments <build-dir> -o <dir> [--kinds k1,k2] [--ref R]")?;
    let out = opt(args, "-o").or_else(|| opt(args, "--out")).context("fragments needs -o <dir>")?;
    let root = fig::engine::RenderRoot::detect(Path::new(build))?;
    let opts = render_opts(args);
    let kinds: Vec<String> = opt(args, "--kinds")
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_else(|| vec!["snapshot".to_string()]);
    let refs: Vec<String> = match opt(args, "--ref") {
        Some(r) => vec![r.to_string()],
        None => fig::engine::own_structure_definitions(&root)?,
    };
    let pairs: Vec<(String, String)> = refs
        .iter()
        .flat_map(|r| kinds.iter().map(move |k| (r.clone(), k.clone())))
        .collect();
    let emitted = fig::engine::emit_fragment_files(&root, &opts, &pairs, Path::new(out))?;
    if !has(args, "--json") {
        eprintln!("fig fragments: emitted {} files to {out}", emitted.len());
    }
    Ok(json!({ "out": out, "emitted": emitted }))
}

// ===========================================================================
// render — THE headline: full static site at Publisher parity
// ===========================================================================
fn cmd_render(args: &[String]) -> Result<Value> {
    let build = positional(args, 2).context("usage: fig render <build-dir> -o <site/> [--generator ts:<adapter.mjs>]")?;
    let out = opt(args, "-o").or_else(|| opt(args, "--out")).context("render needs -o <site/>")?;

    // --generator ts:<adapter.mjs> routes through the bun runner (§3b).
    if let Some(gen) = opt(args, "--generator") {
        return render_via_generator(args, build, out, gen);
    }

    let mut root = fig::engine::RenderRoot::detect(Path::new(build))?;

    // Template story (task #39): `--template <id#ver>` is the DRIVEN default —
    // acquire the chain via the SAME acquisition machinery regular packages use and
    // materialize it with the loader; `--template-dir <dir>` is the explicit
    // pre-materialized escape hatch. Neither → the staged `_includes/` (frozen
    // fallback). The materialize composition lives in `fig::template` (iron rule).
    let template_dir = fig::template::materialized_dir_or_acquire(
        opt(args, "--template"),
        opt(args, "--template-dir"),
        Path::new(build),
        opt(args, "--template-cache").map(Path::new),
        has(args, "--offline"),
    )?;
    if let Some(td) = &template_dir {
        root = root.with_template_dir(td);
    }

    let opts = render_opts(args);
    let outcome = fig::engine::render_site(&root, &opts)?;
    let assets = fig::engine::write_site(&root, &outcome, Path::new(out))?;
    let pages = outcome.pages.len();
    if !has(args, "--json") {
        eprintln!(
            "fig render: {pages} pages, {} fragment materializations, {} total files -> {out}{}",
            outcome.fragment_misses,
            assets,
            template_dir
                .as_ref()
                .map(|d| format!(" (template: {})", d.display()))
                .unwrap_or_default(),
        );
    }
    Ok(json!({
        "out": out,
        "pages": pages,
        "fragmentMisses": outcome.fragment_misses,
        "filesWritten": assets,
        "template": template_dir.as_ref().map(|d| d.display().to_string()),
    }))
}

fn render_via_generator(args: &[String], _build: &str, out: &str, gen: &str) -> Result<Value> {
    let adapter = gen.strip_prefix("ts:").context("--generator must be ts:<adapter.mjs>")?;
    let wasm_dir = opt(args, "--wasm-dir").context("--generator needs --wasm-dir <nodejs-wasm-build>")?;
    let project = opt(args, "--project-json").context("--generator needs --project-json <AdapterProject.json>")?;
    let bundles = opt(args, "--bundles-json").context("--generator needs --bundles-json <Session.init bundles>")?;
    let run = fig::runner::GeneratorRun {
        adapter: Path::new(adapter),
        wasm_dir: Path::new(wasm_dir),
        project_json: Path::new(project),
        bundles_json: Path::new(bundles),
        out_dir: Path::new(out),
    };
    let pages = fig::runner::run_generator(&run)?;
    if !has(args, "--json") {
        eprintln!("fig render --generator: adapter rendered {pages} pages -> {out}");
    }
    Ok(json!({ "out": out, "pages": pages, "generator": adapter }))
}

// ===========================================================================
// watch — incremental dev loop, native twin of the browser editor
// ===========================================================================
fn cmd_watch(args: &[String]) -> Result<Value> {
    let build = positional(args, 2).context("usage: fig watch <build-dir> [--serve <addr>]")?;
    let root = fig::engine::RenderRoot::detect(Path::new(build))?;
    let opts = render_opts(args);
    let state = fig::watch::WatchState::initial(root, opts)?;
    let addr = opt(args, "--serve").map(|a| {
        // Allow ":8080" shorthand -> "127.0.0.1:8080".
        if let Some(rest) = a.strip_prefix(':') { format!("127.0.0.1:{rest}") } else { a.to_string() }
    });
    let poll = opt(args, "--poll-ms").and_then(|s| s.parse().ok()).unwrap_or(300);
    // Blocks until Ctrl-C.
    fig::watch::serve(state, addr.as_deref(), poll)?;
    Ok(json!({ "watched": build }))
}

// ===========================================================================
// helpers
// ===========================================================================
fn render_opts(args: &[String]) -> fig::engine::RenderOptions {
    let mut o = fig::engine::RenderOptions::default();
    if let Some(u) = opt(args, "--run-uuid") {
        o.run_uuid = u.to_string();
    }
    o.active_tables = has(args, "--active-tables");
    o.engine = !has(args, "--no-engine");
    o.engine_first = has(args, "--engine-first");
    o.include_dumps = has(args, "--dumps");
    o
}

fn positional<'a>(args: &'a [String], i: usize) -> Option<&'a str> {
    args.get(i).map(String::as_str).filter(|s| !s.starts_with('-'))
}
fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
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

fn resolve_build_epoch(arg: Option<&str>) -> Result<i64> {
    let raw = match arg {
        Some(v) => v.to_string(),
        None => std::env::var("SOURCE_DATE_EPOCH")
            .context("no build timestamp: pass --build-date <epoch|RFC3339> or set SOURCE_DATE_EPOCH")?,
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
         \x20 packages fetch <i#v> | bundle --cache -o <dir>   Acquire / CDN bundle\n\
         \x20 expand <vs.json> [--resources <r.json>]          Tier-1 VS expansion\n\
         \x20 sitedb <ig> --sushi-out <d> --cache <d> -o <db>  S1-S7 site.db producer\n\
         \x20 fragment <build-dir> <ref> <kind>                Render ONE fragment\n\
         \x20 fragments <build-dir> -o <dir> [--kinds k1,k2]   Materialize fragment files\n\
         \x20 render <build-dir> -o <site/> [--template i#v]  Full static site (Publisher parity)\n\
         \x20                              [--template-dir d] [--generator ts:*]\n\
         \x20 watch <build-dir> [--serve :port]                Incremental dev loop + live-reload\n\
         \x20 version                                          Engine + pins\n\
         \n\
         Add --json to any command for the apiVersion result envelope.",
        env!("CARGO_PKG_VERSION"), API_VERSION,
    );
}
