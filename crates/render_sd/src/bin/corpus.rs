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

use render_sd::context::IgContext;
use render_sd::grid::render_grid;
use render_sd::table::{render_table, TableConfig};
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
        // cycle: the committed publisher build's temp/pages holds the
        // publisher's own post-snapshot SDs (golden-provenance-matched inputs;
        // eliminates the SUSHI-snapshot variance).
        "cycle" => PathBuf::from("/home/jmandel/hobby/periodicity-impl/cycle/temp/pages"),
        _ => panic!("unknown ig {}", ig),
    }
}

fn golden_path(ig: &str, id: &str, suffix: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/render-goldens/{}/fragments/StructureDefinition-{}-{}.xhtml",
        REPO, ig, id, suffix
    ))
}

fn cfg_render(
    mut cfg: TableConfig,
    active_tables: bool,
    sd: &Sd,
    ctx: &IgContext,
    def_file: &str,
) -> String {
    cfg.active_tables = active_tables;
    let (b, _gaps) = render_table(sd, ctx, def_file, &cfg);
    b
}

fn render(
    kind: &str,
    sd: &Sd,
    ctx: Option<&IgContext>,
    run_uuid: &str,
    active_tables: bool,
) -> Option<String> {
    let def_file = format!("StructureDefinition-{}-definitions.html", sd.id());
    let body = match kind {
        "grid" => render_grid(sd, ctx?, &def_file, ""),
        "span" => {
            let mut c = render_sd::span::SpanConfig::span();
            c.active_tables = active_tables;
            render_sd::span::render_span(sd, ctx?, &c)
        }
        "spanall" => {
            let mut c = render_sd::span::SpanConfig::spanall();
            c.active_tables = active_tables;
            render_sd::span::render_span(sd, ctx?, &c)
        }
        "snapshot" => {
            let mut cfg = TableConfig::snapshot(run_uuid);
            cfg.active_tables = active_tables;
            let (b, _gaps) = render_table(sd, ctx?, &def_file, &cfg);
            b
        }
        "snapshot-all" => {
            let mut cfg = TableConfig::snapshot_all(run_uuid);
            cfg.active_tables = active_tables;
            let (b, _gaps) = render_table(sd, ctx?, &def_file, &cfg);
            b
        }
        "snapshot-by-mustsupport" => {
            let mut cfg = TableConfig::snapshot_by_mustsupport(run_uuid);
            cfg.active_tables = active_tables;
            let (b, _gaps) = render_table(sd, ctx?, &def_file, &cfg);
            b
        }
        "snapshot-by-mustsupport-all" => {
            let mut cfg = TableConfig::snapshot_by_mustsupport_all(run_uuid);
            cfg.active_tables = active_tables;
            let (b, _gaps) = render_table(sd, ctx?, &def_file, &cfg);
            b
        }
        "snapshot-by-key" => {
            let mut cfg = TableConfig::snapshot_by_key(run_uuid);
            cfg.active_tables = active_tables;
            let (b, _gaps) = render_table(sd, ctx?, &def_file, &cfg);
            b
        }
        "snapshot-by-key-all" => {
            let mut cfg = TableConfig::snapshot_by_key_all(run_uuid);
            cfg.active_tables = active_tables;
            let (b, _gaps) = render_table(sd, ctx?, &def_file, &cfg);
            b
        }
        "diff" => {
            let mut cfg = TableConfig::diff_view(run_uuid);
            cfg.active_tables = active_tables;
            let (b, _gaps) = render_table(sd, ctx?, &def_file, &cfg);
            b
        }
        "diff-all" => {
            let mut cfg = TableConfig::diff_all(run_uuid);
            cfg.active_tables = active_tables;
            let (b, _gaps) = render_table(sd, ctx?, &def_file, &cfg);
            b
        }
        "snapshot-bindings" => cfg_render(TableConfig::snapshot_bindings(run_uuid), active_tables, sd, ctx?, &def_file),
        "snapshot-bindings-all" => cfg_render(TableConfig::snapshot_bindings_all(run_uuid), active_tables, sd, ctx?, &def_file),
        "snapshot-obligations" => cfg_render(TableConfig::snapshot_obligations(run_uuid), active_tables, sd, ctx?, &def_file),
        "snapshot-obligations-all" => cfg_render(TableConfig::snapshot_obligations_all(run_uuid), active_tables, sd, ctx?, &def_file),
        "diff-bindings" => cfg_render(TableConfig::diff_bindings(run_uuid), active_tables, sd, ctx?, &def_file),
        "diff-bindings-all" => cfg_render(TableConfig::diff_bindings_all(run_uuid), active_tables, sd, ctx?, &def_file),
        "diff-obligations" => cfg_render(TableConfig::diff_obligations(run_uuid), active_tables, sd, ctx?, &def_file),
        "diff-obligations-all" => cfg_render(TableConfig::diff_obligations_all(run_uuid), active_tables, sd, ctx?, &def_file),
        // ---- F4 leaf kinds ----
        "contained-index" | "history" => render_sd::leaf::empty_body(),
        "pseudo-ttl" => render_sd::leaf::pseudo_ttl(),
        "pseudo-xml" => render_sd::leaf::pseudo_xml(),
        "inv" => render_sd::leaf::inv(sd, ctx?, true, render_sd::leaf::GenMode::Snap, true),
        "inv-key" => render_sd::leaf::inv(sd, ctx?, true, render_sd::leaf::GenMode::Key, true),
        "inv-diff" => render_sd::leaf::inv(sd, ctx?, true, render_sd::leaf::GenMode::Diff, true),
        _ => return None,
    };
    Some(wrap_raw(&body))
}

/// Harvest the per-run HTG uuid from any golden snapshot fragment of the IG
/// (documented quirk: HierarchicalTableGenerator.uuid is a per-JVM random).
fn harvest_uuid(ig: &str) -> String {
    let dir = format!("{}/render-goldens/{}/fragments", REPO, ig);
    let Ok(rd) = std::fs::read_dir(&dir) else { return String::new() };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with("-snapshot.xhtml") {
            if let Ok(text) = std::fs::read_to_string(e.path()) {
                if let Some(i) = text.find("  // ") {
                    let rest = &text[i + 5..];
                    if let Some(j) = rest.find('\n') {
                        let cand = &rest[..j];
                        if cand.len() == 36 {
                            return cand.to_string();
                        }
                    }
                }
            }
        }
    }
    String::new()
}

/// The IG's `active-tables` parameter, read from the template's working IG
/// (the file the publisher merged template params into). us-core sets false;
/// the base/davinci templates default true (verified in F0 template dirs).
fn ig_active_tables(ig: &str) -> bool {
    let candidates = match ig {
        "us-core" => vec![format!("{}/us-core/template/onGenerate-ig-working.json", F0), format!("{}/us-core/template/onLoad-ig-working.json", F0)],
        "plan-net" => vec![format!("{}/plan-net/template/onGenerate-ig-working.json", F0), format!("{}/plan-net/template/onLoad-ig-working.json", F0)],
        "cycle" => vec!["/home/jmandel/hobby/periodicity-impl/cycle/template/onGenerate-ig-working.json".to_string()],
        _ => vec![],
    };
    for c in candidates {
        let Ok(text) = std::fs::read_to_string(&c) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        if let Some(params) = v
            .get("definition")
            .and_then(|d| d.get("parameter"))
            .and_then(|p| p.as_array())
        {
            for p in params {
                let code = p.get("code").and_then(|c| {
                    c.as_str()
                        .map(String::from)
                        .or_else(|| c.get("code").and_then(|x| x.as_str()).map(String::from))
                });
                if code.as_deref() == Some("active-tables") {
                    return p.get("value").and_then(|x| x.as_str()) == Some("true");
                }
            }
        }
    }
    false
}

fn build_ctx(ig: &str) -> Option<IgContext> {
    let (own, pkgs, txc) = match ig {
        "us-core" => (
            format!("{}/us-core/output", F0),
            format!("{}/us-core/.home/.fhir/packages", F0),
            Some(format!("{}/us-core/input-cache/txcache", F0)),
        ),
        "plan-net" => (
            format!("{}/plan-net/output", F0),
            format!("{}/plan-net/.home/.fhir/packages", F0),
            Some(format!("{}/plan-net/input-cache/txcache", F0)),
        ),
        "cycle" => (
            "/home/jmandel/hobby/periodicity-impl/cycle/temp/pages".to_string(),
            // cycle's build used the user's global package cache (no isolated
            // HOME — see render-goldens/cycle/PIN.md).
            format!("{}/.fhir/packages", std::env::var("HOME").unwrap_or_default()),
            Some("/home/jmandel/hobby/periodicity-impl/cycle/input-cache/txcache".to_string()),
        ),
        _ => return None,
    };
    Some(IgContext::load_with_txcache(
        Path::new(&own),
        Path::new(&pkgs),
        txc.as_deref().map(Path::new),
    ))
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
    let ctx = build_ctx(ig);
    let run_uuid = harvest_uuid(ig);
    let active_tables = ig_active_tables(ig);
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
        // Quirk-registry: goldens that are publisher error artifacts ("I/O
        // error writing PNG file!" spans) are invalid oracles — the publisher
        // itself failed on them. Skip with a note (2 plan-net snapshots).
        if golden.contains("<span style=\"color:red\">") && golden.len() < 120 {
            eprintln!("  skip {} ({}): golden is a publisher error artifact", id, kind);
            continue;
        }
        let ours = match render(kind, &sd, ctx.as_ref(), &run_uuid, active_tables) {
            Some(o) => o,
            None => {
                eprintln!("unsupported kind {}", kind);
                std::process::exit(2);
            }
        };
        // Optional: dump ours + golden for one id (debug). `--dump <id>` writes
        // dump-ours.xhtml / dump-gold.xhtml under $CORPUS_DUMP_DIR (or std temp).
        if let Some(pos) = args.iter().position(|a| a == "--dump") {
            if args.get(pos + 1).map(|s| s.as_str()) == Some(id.as_str()) {
                let dir = std::env::var("CORPUS_DUMP_DIR")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| std::env::temp_dir());
                std::fs::write(dir.join("dump-ours.xhtml"), &ours).ok();
                std::fs::write(dir.join("dump-gold.xhtml"), &golden).ok();
                eprintln!("dumped {} to {}", id, dir.display());
            }
        }
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
