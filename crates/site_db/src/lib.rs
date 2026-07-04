//! site_db — the cycle site.db producer (Phase 2, task #15).
//!
//! Pipeline S1..S7 (see docs/cycle-package-db-plan.md §2b):
//!   S1/S2  compiler::build_project_with_cache  -> fsh-generated/resources/*.json
//!   S3     snapshot_gen::generate_snapshot     -> snapshot-complete SDs (in place)
//!   S5     rows::*                              -> Resources/Concepts/Metadata rows
//!   S6     augment::augment                     -> Pages/Menu/SiteConfig/Assets rows
//!   S7     writer::write_site_db                -> site.db (rusqlite sink)
//! S4 (ValueSet expansion) is deferred (§4b — cycle needs zero expansions).
//!
//! The row model (`model::SiteDb`) is sqlite-free; only `writer` touches
//! rusqlite (wasm requirement §5). A §2c BuildState ledger runs from day one.

pub mod augment;
pub mod ledger;
pub mod model;
pub mod pipeline;
pub mod rows;
pub mod timefmt;
// The SQLite sink (S7) is native-only; the wasm build (default-features = false)
// gets the row model + in-memory pipeline without it (§5).
#[cfg(feature = "sqlite")]
pub mod writer;

pub use ledger::{BuildLedger, LedgerReport};
pub use model::SiteDb;
pub use pipeline::{
    assemble_rows, build, build_from_inputs, AssembleInputs, BuildConfig, BuildOutcome,
    InMemoryInputs,
};

#[cfg(feature = "sqlite")]
use anyhow::Result;
use std::path::Path;

/// One-shot: run the pipeline and write site.db + the ledger sidecar. Returns the
/// ledger report (for the no-op gate). If a prior ledger sidecar exists next to
/// `out_db`, it is used to classify dirtiness and, on a proven no-op, the site.db
/// write is skipped. Native-only (writes SQLite).
#[cfg(feature = "sqlite")]
pub fn build_and_write(config: &BuildConfig) -> Result<LedgerReport> {
    let ledger_path = ledger_sidecar_path(&config.out_db);
    let prior = BuildLedger::load(&ledger_path);
    let outcome = build(config, prior.as_ref())?;

    if outcome.ledger.no_op && config.out_db.exists() {
        // Proven no-op: nothing changed and the artifact is already present.
        // Write nothing (gate v). Refresh the ledger sidecar bytes (identical).
        outcome.ledger.ledger.save(&ledger_path)?;
        return Ok(outcome.ledger);
    }

    writer::write_site_db(&config.out_db, &outcome.db)?;
    outcome.ledger.ledger.save(&ledger_path)?;
    Ok(outcome.ledger)
}

/// The ledger sidecar path for a given site.db (`<out>.buildstate.json`).
pub fn ledger_sidecar_path(out_db: &Path) -> std::path::PathBuf {
    let mut s = out_db.as_os_str().to_os_string();
    s.push(".buildstate.json");
    std::path::PathBuf::from(s)
}

/// The `site_db` CLI (S1-S7 build). Extracted from the old binary's `main` so
/// BOTH the standalone `site_db` binary AND `fig sitedb` compose the exact same
/// code — byte-identical output. `args` is the full process argv.
#[cfg(feature = "sqlite")]
pub fn run_cli(args: &[String]) -> Result<()> {
    use anyhow::{bail, Context};
    match args.get(1).map(String::as_str) {
        Some("build") => run_build(args),
        Some("--version") | Some("version") => {
            println!("site_db {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        _ => {
            eprintln!(
                "usage: site_db build <cycle-repo> --sushi-out <dir> --cache <pkgcache> --out <site.db>\n\
                 \x20            [--build-date <epoch|RFC3339>] [--core <pkg#ver>] [--no-sushi]\n\
                 \x20            [--branch <b>] [--revision <r>]\n\
                 \x20            [--layer-b | --layer-b-pin | --layer-b-project]  (task #17, default OFF)"
            );
            bail!("unknown or missing subcommand");
        }
    }
}

#[cfg(feature = "sqlite")]
fn run_build(args: &[String]) -> Result<()> {
    use anyhow::Context;
    let opt = |name: &str| -> Option<&str> {
        args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(String::as_str)
    };
    let has_flag = |name: &str| args.iter().any(|a| a == name);

    let ig_dir = args
        .get(2)
        .filter(|s| !s.starts_with('-'))
        .map(std::path::PathBuf::from)
        .context("build needs a <cycle-repo> positional arg")?;
    let sushi_out = opt("--sushi-out").map(std::path::PathBuf::from).context("--sushi-out <dir> is required")?;
    let cache_dir = opt("--cache").map(std::path::PathBuf::from).context("--cache <pkgcache> is required")?;
    let out_db = opt("--out").map(std::path::PathBuf::from).context("--out <site.db> is required")?;
    let core_package = opt("--core").unwrap_or("hl7.fhir.r4.core#4.0.1").to_string();
    let run_sushi = !has_flag("--no-sushi");
    let branch = opt("--branch").map(str::to_string);
    let revision = opt("--revision").map(str::to_string);
    let build_epoch_secs = resolve_build_epoch(opt("--build-date"))?;

    let all = has_flag("--layer-b");
    let layer_b = snapshot_gen::LayerBOptions {
        pin: all || has_flag("--layer-b-pin"),
        project_r4: all || has_flag("--layer-b-project"),
    };

    let config = BuildConfig {
        ig_dir, sushi_out, cache_dir, out_db: out_db.clone(),
        build_epoch_secs, branch, revision, run_sushi, core_package, layer_b,
    };

    let report = build_and_write(&config)?;
    let db_written = !(report.no_op && out_db.exists());
    eprintln!(
        "site_db: {} nodes ({} clean, {} dirty){}",
        report.ledger.nodes.len(), report.clean.len(), report.dirty.len(),
        if report.no_op { " — NO-OP (nothing written)" } else { "" },
    );
    let _ = db_written;
    println!("{}", out_db.display());
    Ok(())
}

#[cfg(feature = "sqlite")]
fn resolve_build_epoch(arg: Option<&str>) -> Result<i64> {
    use anyhow::{bail, Context};
    let raw = match arg {
        Some(v) => v.to_string(),
        None => std::env::var("SOURCE_DATE_EPOCH").context(
            "no build timestamp: pass --build-date <epoch|RFC3339> or set SOURCE_DATE_EPOCH \
             (wall clock is forbidden for determinism, §2c)",
        )?,
    };
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<i64>() {
        return Ok(secs);
    }
    let bytes = raw.as_bytes();
    if bytes.len() >= 19 && bytes[4] == b'-' && bytes[10] == b'T' {
        let y: i64 = raw[0..4].parse()?;
        let mo: i64 = raw[5..7].parse()?;
        let d: i64 = raw[8..10].parse()?;
        let h: i64 = raw[11..13].parse()?;
        let mi: i64 = raw[14..16].parse()?;
        let s: i64 = raw[17..19].parse()?;
        let yy = if mo <= 2 { y - 1 } else { y };
        let era = if yy >= 0 { yy } else { yy - 399 } / 400;
        let yoe = yy - era * 400;
        let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146_097 + doe - 719_468;
        return Ok(days * 86_400 + h * 3600 + mi * 60 + s);
    }
    bail!("unrecognized --build-date '{raw}': want a unix epoch or YYYY-MM-DDThh:mm:ssZ")
}
