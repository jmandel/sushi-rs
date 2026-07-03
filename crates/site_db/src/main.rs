//! site_db CLI:
//!   site_db build <cycle-repo> --sushi-out <dir> --cache <pkgcache> --out site.db
//!                 [--build-date <epoch|RFC3339>] [--core <pkg#ver>]
//!                 [--no-sushi] [--branch <b>] [--revision <r>]
//!
//! --build-date is the injected SOURCE_DATE_EPOCH-style timestamp (never wall
//! clock). Accepts a unix epoch (seconds) or a `YYYY-MM-DDThh:mm:ssZ` string; if
//! omitted it reads $SOURCE_DATE_EPOCH, else errors (determinism, §2c).

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("build") => run_build(&args),
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

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn run_build(args: &[String]) -> Result<()> {
    let ig_dir = args
        .get(2)
        .filter(|s| !s.starts_with('-'))
        .map(PathBuf::from)
        .context("build needs a <cycle-repo> positional arg")?;
    let sushi_out = opt(args, "--sushi-out")
        .map(PathBuf::from)
        .context("--sushi-out <dir> is required")?;
    let cache_dir = opt(args, "--cache")
        .map(PathBuf::from)
        .context("--cache <pkgcache> is required")?;
    let out_db = opt(args, "--out")
        .map(PathBuf::from)
        .context("--out <site.db> is required")?;
    let core_package = opt(args, "--core")
        .unwrap_or("hl7.fhir.r4.core#4.0.1")
        .to_string();
    let run_sushi = !has_flag(args, "--no-sushi");
    let branch = opt(args, "--branch").map(str::to_string);
    let revision = opt(args, "--revision").map(str::to_string);
    let build_epoch_secs = resolve_build_epoch(opt(args, "--build-date"))?;

    // OPT-IN Layer B (task #17). `--layer-b` enables both stages (pin + R4
    // projection); `--layer-b-pin` / `--layer-b-project` toggle them individually.
    // All default OFF: without any flag the pipeline is byte-identical to before.
    let all = has_flag(args, "--layer-b");
    let layer_b = snapshot_gen::LayerBOptions {
        pin: all || has_flag(args, "--layer-b-pin"),
        project_r4: all || has_flag(args, "--layer-b-project"),
    };

    let config = site_db::BuildConfig {
        ig_dir,
        sushi_out,
        cache_dir,
        out_db: out_db.clone(),
        build_epoch_secs,
        branch,
        revision,
        run_sushi,
        core_package,
        layer_b,
    };

    let report = site_db::build_and_write(&config)?;
    let db_written = !(report.no_op && out_db.exists());
    eprintln!(
        "site_db: {} nodes ({} clean, {} dirty){}",
        report.ledger.nodes.len(),
        report.clean.len(),
        report.dirty.len(),
        if report.no_op {
            " — NO-OP (nothing written)"
        } else {
            ""
        }
    );
    let _ = db_written;
    println!("{}", out_db.display());
    Ok(())
}

/// Resolve the injected build timestamp. Precedence: --build-date, then
/// $SOURCE_DATE_EPOCH. Errors if neither is set (never wall clock).
fn resolve_build_epoch(arg: Option<&str>) -> Result<i64> {
    let raw = match arg {
        Some(v) => v.to_string(),
        None => std::env::var("SOURCE_DATE_EPOCH").context(
            "no build timestamp: pass --build-date <epoch|RFC3339> or set SOURCE_DATE_EPOCH \
             (wall clock is forbidden for determinism, §2c)",
        )?,
    };
    parse_build_date(&raw)
}

/// Accept a unix epoch (seconds) or a minimal `YYYY-MM-DDThh:mm:ss[Z]` string.
fn parse_build_date(raw: &str) -> Result<i64> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<i64>() {
        return Ok(secs);
    }
    // Minimal RFC3339 (UTC) parse: YYYY-MM-DDThh:mm:ss with optional Z/offset ignored.
    let bytes = raw.as_bytes();
    if bytes.len() >= 19 && bytes[4] == b'-' && bytes[10] == b'T' {
        let y: i64 = raw[0..4].parse()?;
        let mo: i64 = raw[5..7].parse()?;
        let d: i64 = raw[8..10].parse()?;
        let h: i64 = raw[11..13].parse()?;
        let mi: i64 = raw[14..16].parse()?;
        let s: i64 = raw[17..19].parse()?;
        return Ok(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + s);
    }
    bail!("unrecognized --build-date '{raw}': want a unix epoch or YYYY-MM-DDThh:mm:ssZ")
}

/// days since 1970-01-01 for a civil date (Howard Hinnant).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
