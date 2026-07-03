//! rust_sushi CLI. Phase-by-phase the `compile` subcommand grows; for now it
//! exposes `lex` for token-stream parity checking against the ANTLR oracle.

use fsh_lexer_parser::{import_to_json, lex_document, Channel};
use package_acquisition::{default_registries, Coordinate, PackageCas};

// mimalloc is a native-only speedup: it is a C dependency that cannot compile
// for wasm targets (no libc `wchar.h`). Gate it out for wasm; native behavior is
// unchanged (same global allocator as before on every non-wasm target).
#[cfg(not(target_family = "wasm"))]
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("--version") | Some("version") => {
            println!("rust_sushi {}", env!("CARGO_PKG_VERSION"));
        }
        Some("lex") => {
            // rust_sushi lex <file.fsh>  -> token JSON matching harness/lex-oracle.cjs
            let file = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: rust_sushi lex <file.fsh>"))?;
            let content = std::fs::read_to_string(file)?;
            let toks = lex_document(&content);
            let arr: Vec<serde_json::Value> = toks
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": t.kind.name(),
                        "channel": match t.channel {
                            Channel::Hidden => serde_json::json!("HIDDEN"),
                            Channel::Default => serde_json::json!(0),
                        },
                        "text": t.text,
                        "line": t.line,
                        "col": t.col,
                        "start": t.start,
                        "stop": t.stop,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&arr)?);
        }
        Some("ast") => {
            // rust_sushi ast <file.fsh>  -> import AST JSON matching harness/parse-oracle.cjs
            let file = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: rust_sushi ast <file.fsh>"))?;
            let content = std::fs::read_to_string(file)?;
            let v = import_to_json(&[(file.as_str(), &content)]);
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Some("expand") => {
            // rust_sushi expand <file.fsh ...>  -> post-expansion AST matching harness/expand-oracle.cjs
            let files: Vec<String> = args[2..].to_vec();
            if files.is_empty() {
                return Err(anyhow::anyhow!("usage: rust_sushi expand <file.fsh ...>"));
            }
            let loaded: Vec<(String, String)> = files
                .iter()
                .map(|f| Ok((f.clone(), std::fs::read_to_string(f)?)))
                .collect::<anyhow::Result<_>>()?;
            let refs: Vec<(&str, &str)> = loaded
                .iter()
                .map(|(p, c)| (p.as_str(), c.as_str()))
                .collect();
            let v = compiler::expand_to_json(&refs);
            println!("{}", serde_json::to_string_pretty(&v)?);
        }
        Some("build") => {
            // rust_sushi build <ig-dir> -o <out-dir> [--cache <cache-dir> | --materialize]
            let ig = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: rust_sushi build <ig-dir> -o <out>"))?;
            let mut out = "fsh-generated".to_string();
            if let Some(i) = args.iter().position(|a| a == "-o" || a == "--out") {
                out = args
                    .get(i + 1)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("-o needs a value"))?;
            }
            let cache = option_value(&args, "--cache");
            if has_flag(&args, "--materialize") {
                let cas = PackageCas::new(
                    option_value(&args, "--cas")
                        .map_or(PackageCas::default_root()?, std::path::PathBuf::from),
                );
                let registries = option_value(&args, "--registry")
                    .map(|r| vec![r.to_string()])
                    .unwrap_or_else(default_registries);
                let lock_path = option_value(&args, "--lock")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::Path::new(ig).join("fhir-deps.lock"));
                let cache_dir = cache.map(std::path::PathBuf::from).unwrap_or_else(|| {
                    std::path::Path::new(&out)
                        .join(".rust_sushi")
                        .join("fhir-cache")
                });
                let offline = has_flag(&args, "--offline");
                if lock_path.is_file() {
                    cas.materialize_lock_with_options(&lock_path, &cache_dir, offline)?;
                } else {
                    if offline {
                        return Err(anyhow::anyhow!(
                            "{} does not exist and --offline is set",
                            lock_path.display()
                        ));
                    }
                    cas.lock_project_with_options(
                        ig,
                        &lock_path,
                        &registries,
                        false,
                        &[],
                        offline,
                    )?;
                    cas.materialize_lock_with_options(&lock_path, &cache_dir, offline)?;
                }
                compiler::build_project_with_cache(ig, &out, &cache_dir.to_string_lossy())?;
            } else if let Some(cache) = cache {
                compiler::build_project_with_cache(ig, &out, cache)?;
            } else {
                compiler::build_project(ig, &out)?;
            }
        }
        Some("cas") => match args.get(2).map(String::as_str) {
            Some("acquire") => {
                let coord = args.get(3).ok_or_else(|| {
                        anyhow::anyhow!("usage: rust_sushi cas acquire <name#version> [--cas <dir>] [--registry <url>]")
                    })?;
                let coord = Coordinate::parse(coord)?;
                let cas = PackageCas::new(
                    option_value(&args, "--cas")
                        .map_or(PackageCas::default_root()?, std::path::PathBuf::from),
                );
                let registries = option_value(&args, "--registry")
                    .map(|r| vec![r.to_string()])
                    .unwrap_or_else(default_registries);
                let package_ref = cas.acquire_remote(&coord, &registries)?;
                println!("{}", serde_json::to_string_pretty(&package_ref)?);
            }
            Some("ingest") => {
                let coord = args.get(3).ok_or_else(|| {
                        anyhow::anyhow!("usage: rust_sushi cas ingest <name#version> <path.tgz|package-dir> [--cas <dir>]")
                    })?;
                let source = args.get(4).ok_or_else(|| {
                        anyhow::anyhow!("usage: rust_sushi cas ingest <name#version> <path.tgz|package-dir> [--cas <dir>]")
                    })?;
                let coord = Coordinate::parse(coord)?;
                let cas = PackageCas::new(
                    option_value(&args, "--cas")
                        .map_or(PackageCas::default_root()?, std::path::PathBuf::from),
                );
                let package_ref = cas.ingest_local_source(&coord, source)?;
                println!("{}", serde_json::to_string_pretty(&package_ref)?);
            }
            _ => {
                return Err(anyhow::anyhow!(
                        "usage: rust_sushi cas <acquire <name#version> | ingest <name#version> <path>> [--cas <dir>]"
                    ));
            }
        },
        Some("deps") => match args.get(2).map(String::as_str) {
            Some("lock") | Some("update") => {
                let project = option_value(&args, "--project").ok_or_else(|| {
                    anyhow::anyhow!(
                        "usage: rust_sushi deps <lock|update> --project <ig> [--lock <file>] [--cas <dir>] [--registry <url>] [--offline] [--all-mutable|<package-id>...]"
                    )
                })?;
                let lock_path = option_value(&args, "--lock")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| std::path::Path::new(project).join("fhir-deps.lock"));
                let cas = PackageCas::new(
                    option_value(&args, "--cas")
                        .map_or(PackageCas::default_root()?, std::path::PathBuf::from),
                );
                let registries = option_value(&args, "--registry")
                    .map(|r| vec![r.to_string()])
                    .unwrap_or_else(default_registries);
                let update_all = args.get(2).map(String::as_str) == Some("update")
                    && has_flag(&args, "--all-mutable");
                let update_packages = positional_args(
                    &args,
                    3,
                    &["--project", "--lock", "--cas", "--registry"],
                    &["--all-mutable", "--offline"],
                );
                if args.get(2).map(String::as_str) == Some("update")
                    && !update_all
                    && update_packages.is_empty()
                {
                    return Err(anyhow::anyhow!(
                        "usage: rust_sushi deps update --project <ig> (--all-mutable | <package-id>...) [--lock <file>] [--cas <dir>]"
                    ));
                }
                let lock = cas.lock_project_with_options(
                    project,
                    &lock_path,
                    &registries,
                    update_all,
                    &update_packages,
                    has_flag(&args, "--offline"),
                )?;
                println!("{}", serde_json::to_string_pretty(&lock)?);
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "usage: rust_sushi deps <lock|update> --project <ig> ..."
                ));
            }
        },
        Some("materialize") => {
            let out = option_value(&args, "--out")
                .or_else(|| option_value(&args, "-o"))
                .ok_or_else(|| anyhow::anyhow!("materialize needs --out <cache-dir>"))?;
            let cas = PackageCas::new(
                option_value(&args, "--cas")
                    .map_or(PackageCas::default_root()?, std::path::PathBuf::from),
            );
            let offline = has_flag(&args, "--offline");
            if let Some(lock_path) = option_value(&args, "--lock") {
                let lock = cas.materialize_lock_with_options(lock_path, out, offline)?;
                println!("{}", serde_json::to_string_pretty(&lock)?);
            } else if let Some(project) = option_value(&args, "--project") {
                let lock_path = std::path::Path::new(project).join("fhir-deps.lock");
                let registries = option_value(&args, "--registry")
                    .map(|r| vec![r.to_string()])
                    .unwrap_or_else(default_registries);
                let lock = if lock_path.is_file() {
                    cas.materialize_lock_with_options(&lock_path, out, offline)?
                } else {
                    if offline {
                        return Err(anyhow::anyhow!(
                            "{} does not exist and --offline is set",
                            lock_path.display()
                        ));
                    }
                    let lock = cas.lock_project_with_options(
                        project,
                        &lock_path,
                        &registries,
                        false,
                        &[],
                        false,
                    )?;
                    cas.materialize_lock_with_options(&lock_path, out, offline)?;
                    lock
                };
                println!("{}", serde_json::to_string_pretty(&lock)?);
            } else {
                let coord = option_value(&args, "--package").ok_or_else(|| {
                    anyhow::anyhow!(
                        "usage: rust_sushi materialize (--package <name#version> | --lock <file> | --project <ig>) --out <cache-dir> [--cas <dir>] [--offline]"
                    )
                })?;
                let coord = Coordinate::parse(coord)?;
                let registries = option_value(&args, "--registry")
                    .map(|r| vec![r.to_string()])
                    .unwrap_or_else(default_registries);
                let package_ref =
                    cas.materialize_package_resolving(&coord, out, &registries, offline)?;
                println!("{}", serde_json::to_string_pretty(&package_ref)?);
            }
        }
        Some("pkg-fish") => {
            // rust_sushi pkg-fish <ig-dir> <cache-dir> <query...>  -> package-oracle JSON shape
            let ig = args
                .get(2)
                .ok_or_else(|| anyhow::anyhow!("usage: pkg-fish <ig> <cache> <query...>"))?;
            let cache = args
                .get(3)
                .ok_or_else(|| anyhow::anyhow!("need <cache-dir>"))?;
            let queries = &args[4.min(args.len())..];
            let store = package_store::PackageStore::for_project(ig, cache)?;
            let mut qout = Vec::new();
            for q in queries {
                let fhir = store.fish_for_fhir(q, package_store::ALL_FISH_TYPES);
                let meta = store.fish_for_metadata(q, package_store::ALL_FISH_TYPES);
                let fhir_summary = fhir.as_ref().map(|v| {
                    serde_json::json!({
                        "resourceType": v.get("resourceType"), "id": v.get("id"),
                        "url": v.get("url"), "version": v.get("version"),
                    })
                });
                qout.push(serde_json::json!({"query": q, "fhir": fhir_summary, "meta": meta}));
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"queries": qout}))?
            );
        }
        _ => {
            eprintln!(
                "rust_sushi {}: compile pipeline under construction",
                env!("CARGO_PKG_VERSION")
            );
            eprintln!(
                "usage: rust_sushi <lex|ast|expand|build|cas|materialize|pkg-fish|--version> ..."
            );
        }
    }
    Ok(())
}

fn option_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn positional_args(
    args: &[String],
    start: usize,
    value_options: &[&str],
    flag_options: &[&str],
) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = start;
    while i < args.len() {
        let arg = &args[i];
        if value_options.iter().any(|opt| opt == arg) {
            i += 2;
        } else if flag_options.iter().any(|opt| opt == arg) {
            i += 1;
        } else if arg.starts_with('-') {
            i += 1;
        } else {
            out.push(arg.clone());
            i += 1;
        }
    }
    out
}
