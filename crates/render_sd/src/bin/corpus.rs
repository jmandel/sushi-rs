//! corpus: run one fragment KIND across an IG's SDs, diffing our output against
//! the committed golden, and report per-kind pass/total with first-divergence
//! context for failures.
//!
//! Usage: corpus <kind> <ig> [--verbose]
//!   ig: cycle | plan-net | us-core
//! Inputs: snapshot-complete SDs from the F0 build's output/ dir (us-core,
//! plan-net) or the render-goldens fixtures (cycle); goldens from
//! render-goldens/<ig>/fragments/StructureDefinition-<id>-<kind>.xhtml.

use std::path::{Path, PathBuf};

use render_sd::grid::render_grid;
use render_sd::{wrap_raw, Sd};

const REPO: &str = "/home/jmandel/hobby/sushi-rs-snapshot";
const F0: &str = "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds";

fn ig_sd_dir(ig: &str) -> PathBuf {
    match ig {
        "us-core" => PathBuf::from(format!("{}/us-core/output", F0)),
        "plan-net" => PathBuf::from(format!("{}/plan-net/output", F0)),
        // cycle: no F0 build exists; use the sushi fsh-generated snapshots from
        // the periodicity-impl checkout. NOTE: these snapshots are SUSHI-made,
        // not publisher-regenerated, so snapshot-source variance is possible for
        // cycle (documented in the worklog). Prefer an F0 build if present.
        "cycle" => {
            let f0 = PathBuf::from(format!("{}/cycle/output", F0));
            if f0.exists() {
                f0
            } else {
                PathBuf::from(
                    "/home/jmandel/hobby/periodicity-impl/cycle/fsh-generated/resources",
                )
            }
        }
        _ => panic!("unknown ig {}", ig),
    }
}

fn golden_path(ig: &str, id: &str, suffix: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/render-goldens/{}/fragments/StructureDefinition-{}-{}.xhtml",
        REPO, ig, id, suffix
    ))
}

fn render(kind: &str, sd: &Sd) -> Option<String> {
    let def_file = format!("StructureDefinition-{}-definitions.html", sd.id());
    let body = match kind {
        "grid" => render_grid(sd, &def_file, ""),
        _ => return None,
    };
    Some(wrap_raw(&body))
}

fn first_divergence(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: corpus <kind> <ig> [--verbose]");
        std::process::exit(2);
    }
    let kind = &args[1];
    let ig = &args[2];
    let verbose = args.iter().any(|a| a == "--verbose");

    let sd_dir = ig_sd_dir(ig);
    let mut pass = 0;
    let mut total = 0;
    let mut fails: Vec<(String, usize, usize)> = Vec::new();
    let missing_golden = 0;

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&sd_dir)
        .unwrap_or_else(|_| panic!("read dir {}", sd_dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("StructureDefinition-") && n.ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();
    entries.sort();

    for path in entries {
        let json = match std::fs::read_to_string(&path) {
            Ok(j) => j,
            Err(_) => continue,
        };
        let sd = match Sd::from_json(&json) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !sd.has_snapshot() {
            continue;
        }
        let id = sd.id().to_string();
        let gp = golden_path(ig, &id, kind);
        if !gp.exists() {
            continue; // this SD does not produce this fragment kind
        }
        let golden = std::fs::read_to_string(&gp).unwrap();
        let ours = match render(kind, &sd) {
            Some(o) => o,
            None => {
                eprintln!("unsupported kind {}", kind);
                std::process::exit(2);
            }
        };
        total += 1;
        if ours == golden {
            pass += 1;
        } else {
            let d = first_divergence(&ours, &golden);
            fails.push((id.clone(), d, golden.len()));
            if verbose {
                report_diff(&id, &ours, &golden, d);
            }
        }
        let _ = missing_golden;
    }

    println!("{} {}: {}/{} byte-identical", kind, ig, pass, total);
    if !fails.is_empty() {
        println!("  {} failures (id, first-divergence-byte, golden-len):", fails.len());
        for (id, d, len) in fails.iter().take(20) {
            println!("    {} @ {} / {}", id, d, len);
        }
    }
}

fn report_diff(id: &str, ours: &str, golden: &str, d: usize) {
    let ctx = 80;
    let lo = d.saturating_sub(ctx);
    let show = |s: &str| -> String {
        let end = (d + ctx).min(s.len());
        s.get(lo..end).unwrap_or("").replace('\n', "\\n")
    };
    println!("--- {} first divergence @ byte {} ---", id, d);
    println!("  OURS  : ...{}", show(ours));
    println!("  GOLDEN: ...{}", show(golden));
}

#[allow(dead_code)]
fn _p(_: &Path) {}
