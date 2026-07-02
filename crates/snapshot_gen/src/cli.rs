//! Command-line entry points: argument parsing, usage, batch runner, and the
//! `Engine` selector plumbed into the generator.

#![allow(unused_imports)]
use anyhow::{bail, Context};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::*;

#[derive(Clone, Debug)]
pub struct SnapshotOptions {
    pub sort_differential: bool,
    pub native_r5: bool,
    /// Apply `checkExtensionDoco` to an extension profile's own untouched root.
    /// Java only normalizes the root of the profile being generated, never a
    /// dependency extension consumed elsewhere as a slice/overlay source, so this
    /// is true only for the top-level entry point and false for recursive calls.
    pub apply_extension_root_doco: bool,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            sort_differential: true,
            native_r5: false,
            apply_extension_root_doco: false,
        }
    }
}

/// Snapshot-generation engine selector. `Legacy` is the current diff-order patch
/// engine (default until the walk engine reaches parity); `Walk` is the upcoming
/// decision-isomorphic engine (not implemented yet). Threaded through the CLI
/// entry points so the walk engine can slot in behind `run_engine` without
/// touching callers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Engine {
    Legacy,
    Walk,
}

impl Default for Engine {
    fn default() -> Self {
        Engine::Legacy
    }
}

impl Engine {
    pub(crate) fn parse(value: &str) -> anyhow::Result<Engine> {
        match value {
            "legacy" => Ok(Engine::Legacy),
            "walk" => Ok(Engine::Walk),
            other => bail!("unknown --engine value: {other} (expected `legacy` or `walk`)"),
        }
    }
}

/// Generate a snapshot with the selected engine. The legacy engine delegates to
/// `generate_snapshot` (unchanged public API); the walk engine is not yet
/// implemented and returns a clear error.
pub(crate) fn run_engine(
    engine: Engine,
    derived: Value,
    ctx: &PackageContext,
    options: SnapshotOptions,
) -> anyhow::Result<Value> {
    match engine {
        Engine::Legacy => generate_snapshot(derived, ctx, options),
        Engine::Walk => bail!("the `walk` engine is not implemented yet"),
    }
}

pub fn main_cli() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut cache: Option<String> = None;
    let mut packages: Vec<String> = Vec::new();
    let mut local_dirs: Vec<String> = Vec::new();
    let mut sort_differential = true;
    let mut native_r5 = false;
    let mut batch_list: Option<String> = None;
    let mut input: Option<String> = None;
    let mut engine: Option<String> = None;
    let mut dump_converted: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
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
            "--engine" => engine = Some(args.next().context("--engine needs <legacy|walk>")?),
            "--sort" => sort_differential = true,
            "--no-sort" | "--direct" => sort_differential = false,
            "--native-r5" | "--output-r5" => native_r5 = true,
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

    // Engine resolution: explicit --engine flag, else the ENGINE env var, else the
    // default (legacy). An empty ENGINE is treated as unset so a plain
    // `ENGINE=` in the environment keeps current behavior.
    let engine = match engine.or_else(|| std::env::var("ENGINE").ok().filter(|v| !v.is_empty())) {
        Some(value) => Engine::parse(&value)?,
        None => Engine::default(),
    };

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
    let options = SnapshotOptions {
        sort_differential,
        native_r5,
        apply_extension_root_doco: true,
    };
    if let Some(batch_list) = batch_list {
        return run_batch_list(&batch_list, &ctx, options, engine);
    }
    let input = input.expect("checked above");
    let source = std::fs::read_to_string(&input)?;
    let derived: Value = serde_json::from_str(&source)?;
    let out = run_engine(engine, derived, &ctx, options)?;
    print!("{}", json_emit::to_fhir_json_string(&out));
    Ok(())
}

pub(crate) fn print_usage() {
    eprintln!(
        "usage: snapshot_gen [--cache <packages-dir>] [--package <pkg#ver> ...] [--local-dir <dir> ...] [--engine <legacy|walk>] [--sort|--no-sort] [--native-r5] [--dump-converted <input.json>] [--batch-list <tsv>] <StructureDefinition.json>"
    );
}

pub(crate) fn run_batch_list(
    batch_list: &str,
    ctx: &PackageContext,
    options: SnapshotOptions,
    engine: Engine,
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
            let out = run_engine(engine, derived, ctx, options.clone())?;
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
