//! Command-line entry points: argument parsing, usage, and the batch runner.
//! Single walk engine; the snapshot output is always the Publisher-native R5
//! internal model.

use anyhow::{bail, Context};
use serde_json::Value;
use std::path::Path;

use crate::*;

#[derive(Clone, Debug)]
pub struct SnapshotOptions {
    /// Run the differential normalizer (sortDifferential + preprocess) before the
    /// walk. True in normal use; `--direct`/`--no-sort` disables it for oracle
    /// trace debugging.
    pub sort_differential: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            sort_differential: true,
        }
    }
}

pub fn main_cli() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut cache: Option<String> = None;
    let mut packages: Vec<String> = Vec::new();
    let mut local_dirs: Vec<String> = Vec::new();
    let mut sort_differential = true;
    let mut batch_list: Option<String> = None;
    let mut input: Option<String> = None;
    let mut dump_converted: Option<String> = None;
    let mut trace: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--trace" => trace = args.next(),
            "--dump-converted" => {
                dump_converted = Some(args.next().context("--dump-converted needs <input.json>")?);
            }
            "--cache" => cache = args.next(),
            "--package" | "-p" => {
                packages.push(args.next().context("--package needs pkg#ver")?);
            }
            "--local-dir" => {
                local_dirs.push(args.next().context("--local-dir needs a directory")?);
            }
            "--sort" => sort_differential = true,
            "--no-sort" | "--direct" => sort_differential = false,
            "--batch-list" => batch_list = args.next(),
            "-h" | "--help" => {
                print_usage();
                return Ok(());
            }
            _ if arg.starts_with('-') => bail!("unknown option: {arg}"),
            _ => input = Some(arg),
        }
    }

    // Stage-2-only mode: pure R4->R5 StructureDefinition conversion, no package
    // context / base / snapshot generation (context-free). Emits the converted
    // JSON so oracle diffs are scriptable.
    if let Some(dump_input) = dump_converted {
        let source = std::fs::read_to_string(&dump_input)
            .with_context(|| format!("failed to read {dump_input}"))?;
        let sd: Value = serde_json::from_str(&source)?;
        let converted = crate::convert_r4_sd_to_r5(&sd)?;
        print!("{}", json_emit::to_fhir_json_string(&converted));
        return Ok(());
    }

    if batch_list.is_none() && input.is_none() {
        bail!("missing input StructureDefinition JSON");
    }
    if packages.is_empty() {
        packages.push("hl7.fhir.r5.core#5.0.0".to_string());
    }
    let cache = cache
        .or_else(|| std::env::var("FHIR_CACHE").ok())
        .unwrap_or_else(|| "temp/fhir-home/.fhir/packages".to_string());
    let mut ctx = PackageContext::new(&cache, &packages)?;
    for local_dir in local_dirs {
        ctx.load_local_dir(local_dir)?;
    }
    // Trace: enabled via --trace <file> or SNAPSHOT_TRACE=<file>. Zero overhead
    // when unset.
    if let Some(trace_path) = trace.or_else(|| {
        std::env::var("SNAPSHOT_TRACE")
            .ok()
            .filter(|v| !v.is_empty())
    }) {
        crate::enable_trace(&trace_path)
            .with_context(|| format!("failed to open trace file {trace_path}"))?;
    }
    let options = SnapshotOptions { sort_differential };
    if let Some(batch_list) = batch_list {
        return run_batch_list(&batch_list, &ctx, options);
    }
    let input = input.expect("checked above");
    let source = std::fs::read_to_string(&input)?;
    let derived: Value = serde_json::from_str(&source)?;
    let out = generate_snapshot(derived, &ctx, options)?;
    print!("{}", json_emit::to_fhir_json_string(&out));
    Ok(())
}

pub(crate) fn print_usage() {
    eprintln!(
        "usage: snapshot_gen [--cache <packages-dir>] [--package <pkg#ver> ...] [--local-dir <dir> ...] [--sort|--no-sort|--direct] [--dump-converted <input.json>] [--trace <file>] [--batch-list <tsv>] <StructureDefinition.json>"
    );
}

pub(crate) fn run_batch_list(
    batch_list: &str,
    ctx: &PackageContext,
    options: SnapshotOptions,
) -> anyhow::Result<()> {
    let source = std::fs::read_to_string(batch_list)
        .with_context(|| format!("failed to read batch list {batch_list}"))?;
    let mut total = 0usize;
    let mut ok = 0usize;
    let mut failed = 0usize;
    for (line_index, line) in source.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        total += 1;
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            failed += 1;
            eprintln!(
                "FAIL rust malformed batch line {}: {}",
                line_index + 1,
                line
            );
            continue;
        }
        let input = parts[0];
        let output = parts[1];
        let name = Path::new(input)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(input);
        let result = (|| -> anyhow::Result<()> {
            let source = std::fs::read_to_string(input)?;
            let derived: Value = serde_json::from_str(&source)?;
            let out = generate_snapshot(derived, ctx, options.clone())?;
            if let Some(parent) = Path::new(output).parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(output, json_emit::to_fhir_json_string(&out))?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                ok += 1;
                println!("OK rust {name}");
            }
            Err(err) => {
                failed += 1;
                let _ = std::fs::remove_file(output);
                eprintln!("FAIL rust {name}: {err:#}");
            }
        }
    }
    println!("RUST BATCH: ok={ok} failed={failed} total={total}");
    if failed != 0 {
        bail!("Rust batch had {failed} failures");
    }
    Ok(())
}
