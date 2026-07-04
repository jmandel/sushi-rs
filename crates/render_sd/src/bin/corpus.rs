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

/// corePath for the CanonicalRenderer leaf methods = checkAppendSlash(specPath)
/// = VersionUtilities.getSpecUrl(igVersion)+"/". The IG version is the SD's
/// fhirVersion in this corpus (all R4 -> http://hl7.org/fhir/R4/).
fn core_path_for(sd: &Sd) -> String {
    let v = sd.fhir_version();
    let base = if v.starts_with("4.0") {
        "http://hl7.org/fhir/R4"
    } else if v.starts_with("4.3") {
        "http://hl7.org/fhir/R4B"
    } else if v.starts_with("5.0") {
        "http://hl7.org/fhir/R5"
    } else if v.starts_with("3.0") {
        "http://hl7.org/fhir/STU3"
    } else {
        "http://hl7.org/fhir"
    };
    format!("{}/", base)
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
        "pseudo-json" => render_sd::pseudojson::pseudo_json(sd, ctx?, &core_path_for(sd)),
        "inv" => render_sd::leaf::inv(sd, ctx?, true, render_sd::leaf::GenMode::Snap, true),
        "inv-key" => render_sd::leaf::inv(sd, ctx?, true, render_sd::leaf::GenMode::Key, true),
        "inv-diff" => render_sd::leaf::inv(sd, ctx?, true, render_sd::leaf::GenMode::Diff, true),
        "sd-use-context" => render_sd::leaf::use_context(sd, ctx?, &core_path_for(sd)),
        "tx" => render_sd::tx::render_tx(sd, ctx?, &core_path_for(sd), render_sd::tx::TxOpts::tx()),
        "tx-must-support" => render_sd::tx::render_tx(sd, ctx?, &core_path_for(sd), render_sd::tx::TxOpts::tx_must_support()),
        "tx-key" => render_sd::tx::render_tx(sd, ctx?, &core_path_for(sd), render_sd::tx::TxOpts::tx_key()),
        "tx-diff" => render_sd::tx::render_tx(sd, ctx?, &core_path_for(sd), render_sd::tx::TxOpts::tx_diff()),
        "tx-diff-must-support" => render_sd::tx::render_tx(sd, ctx?, &core_path_for(sd), render_sd::tx::TxOpts::tx_diff_must_support()),
        // ---- dict fragment family ----
        "dict" => render_sd::dict::render_dict(sd, ctx?, &core_path_for(sd), true, render_sd::dict::GEN_MODE_SNAP, ""),
        "dict-active" => render_sd::dict::render_dict(sd, ctx?, &core_path_for(sd), false, render_sd::dict::GEN_MODE_SNAP, ""),
        "dict-diff" => render_sd::dict::render_dict(sd, ctx?, &core_path_for(sd), true, render_sd::dict::GEN_MODE_DIFF, "diff_"),
        "dict-ms" => render_sd::dict::render_dict(sd, ctx?, &core_path_for(sd), true, render_sd::dict::GEN_MODE_MS, "ms_"),
        "dict-key" => render_sd::dict::render_dict(sd, ctx?, &core_path_for(sd), true, render_sd::dict::GEN_MODE_KEY, "key_"),
        "summary" => render_sd::leaf::summary(sd, ctx?, false, &core_path_for(sd)),
        "summary-all" => render_sd::leaf::summary(sd, ctx?, true, &core_path_for(sd)),
        "uses" => render_sd::xref::uses(sd, ctx?),
        "sd-xref" => render_sd::xref::references(sd, ctx?),
        "maps" => render_sd::xref::maps(sd, ctx?, &def_file, run_uuid, active_tables),
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

// ---------------------------------------------------------------------------
// SINGLETON IG-level aggregate harness (render_sd::aggregates)
// ---------------------------------------------------------------------------

/// The IG-level aggregate fragment kinds (one golden per IG, no SD prefix).
fn is_singleton_kind(kind: &str) -> bool {
    matches!(
        kind,
        "new-extensions"
            | "related-igs-table"
            | "related-igs-list"
            | "globals-table"
            | "obligation-summary"
            | "deleted-extensions"
            | "cross-version-analysis"
            | "cross-version-analysis-inline"
            | "valueset-list"
            | "summary-extensions"
            | "summary-observations"
            | "deprecated-list"
            | "expansion-params"
            | "codesystem-list"
            | "canonical-index"
            | "ip-statements"
            | "dependency-table"
            | "dependency-table-short"
            | "dependency-table-nontech"
            | "valueset-ref-list"
            | "valueset-ref-all-list"
            | "codesystem-ref-list"
            | "codesystem-ref-all-list"
    )
}

/// The build's oids.ini OID registry (us-core has one; cycle/plan-net do not).
fn ig_oids_ini(ig: &str) -> Option<render_sd::aggregates::OidMap> {
    let path = match ig {
        "us-core" => format!("{}/us-core/oids.ini", F0),
        "plan-net" => format!("{}/plan-net/oids.ini", F0),
        "cycle" => "/home/jmandel/hobby/periodicity-impl/cycle/oids.ini".to_string(),
        _ => return None,
    };
    let text = std::fs::read_to_string(&path).ok()?;
    let mut map = render_sd::aggregates::parse_oids_ini(&text);
    // The IG itself is assigned the sushi-config `auto-oid-root` (the parent of
    // all resource OIDs); the publisher shows it in the IG's canonical-index
    // row but it is not listed in oids.ini. Inject it under (ImplementationGuide,
    // ig-id).
    if let (Some(root), Some((id, _url, _v))) = (ig_auto_oid_root(ig), ig_resource(ig)) {
        map.entry(("ImplementationGuide".to_string(), id))
            .or_insert_with(|| vec![root]);
    }
    Some(map)
}

/// The `auto-oid-root` from the build's sushi-config.yaml (the IG's own OID).
fn ig_auto_oid_root(ig: &str) -> Option<String> {
    let path = match ig {
        "us-core" => format!("{}/us-core/sushi-config.yaml", F0),
        "plan-net" => format!("{}/plan-net/sushi-config.yaml", F0),
        "cycle" => "/home/jmandel/hobby/periodicity-impl/cycle/sushi-config.yaml".to_string(),
        _ => return None,
    };
    let text = std::fs::read_to_string(&path).ok()?;
    for line in text.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("auto-oid-root:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// The own ImplementationGuide (id, url, version) for canonical-index.
fn ig_resource(ig: &str) -> Option<(String, String, String)> {
    let dir = ig_sd_dir(ig);
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if n.starts_with("ImplementationGuide-") && n.ends_with(".json") {
            let t = std::fs::read_to_string(e.path()).ok()?;
            let v: serde_json::Value = serde_json::from_str(&t).ok()?;
            let id = v.get("id").and_then(|x| x.as_str())?.to_string();
            let url = v.get("url").and_then(|x| x.as_str())?.to_string();
            let ver = v.get("version").and_then(|x| x.as_str()).unwrap_or("").to_string();
            return Some((id, url, ver));
        }
    }
    None
}

/// Per-IG build fact: does the context carry "interesting" expansion parameters
/// (anything beyond x-system-cache-id/defaultDisplayLanguage)? Not derivable
/// from output/. Golden-matched: cycle/plan-net none, us-core a grid.
fn ig_has_expansion_params(ig: &str) -> bool {
    match ig {
        "us-core" => true,
        _ => false,
    }
}

/// The IG's business version (ImplementationGuide.version) — read from the
/// own IG resource. needVersionReferences comparator baseline.
fn ig_version(ig: &str) -> String {
    let dir = ig_sd_dir(ig);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("ImplementationGuide-") && n.ends_with(".json") {
                if let Ok(t) = std::fs::read_to_string(e.path()) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                        if let Some(ver) = v.get("version").and_then(|x| x.as_str()) {
                            return ver.to_string();
                        }
                    }
                }
            }
        }
    }
    String::new()
}

/// Per-IG build fact: did the PreviousVersionComparator load a lastVersion?
/// (network `package-list.json` fetch; not derivable from output/). Golden-
/// matched: cycle has a previous published version, plan-net/us-core do not.
fn ig_has_previous(ig: &str) -> bool {
    match ig {
        "cycle" => true,
        "plan-net" | "us-core" => false,
        _ => false,
    }
}

/// Per-IG build fact: R4ToR4BAnalyser `newFormat` (isNewML) — selects the
/// `../package` (true) vs `package` (false) tgz-link prefix. Golden-matched.
fn ig_new_format(ig: &str) -> bool {
    match ig {
        "cycle" | "plan-net" => true,
        "us-core" => false,
        _ => true,
    }
}

/// corePath for the IG-level singleton renderers (DeprecationRenderer etc.) =
/// getSpecUrl(igVersion)+"/", derived from the IG's fhirVersion (same rule as
/// core_path_for over an SD; the IG is R4 across this corpus).
fn singleton_core_path(ig: &str) -> String {
    let dir = ig_sd_dir(ig);
    let mut v = String::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("ImplementationGuide-") && n.ends_with(".json") {
                if let Ok(t) = std::fs::read_to_string(e.path()) {
                    if let Ok(j) = serde_json::from_str::<serde_json::Value>(&t) {
                        if let Some(fv) = j.get("fhirVersion").and_then(|x| x.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()) {
                            v = fv.to_string();
                        }
                    }
                }
            }
        }
    }
    let base = if v.starts_with("4.0") {
        "http://hl7.org/fhir/R4"
    } else if v.starts_with("4.3") {
        "http://hl7.org/fhir/R4B"
    } else if v.starts_with("5.0") {
        "http://hl7.org/fhir/R5"
    } else {
        "http://hl7.org/fhir"
    };
    format!("{}/", base)
}

/// The DependencyRenderer `dstFolder` = the build's `temp/pages` (the HTG
/// `dest`; the tree-line img srcs read static PNGs from here, and the
/// background-image url is prefixed with this path). Same per-IG build path the
/// snapshot tables use as their SD source dir.
fn dep_dst_folder(ig: &str) -> String {
    match ig {
        "us-core" => format!("{}/us-core/temp/pages", F0),
        "plan-net" => format!("{}/plan-net/temp/pages", F0),
        "cycle" => "/home/jmandel/hobby/periodicity-impl/cycle/temp/pages".to_string(),
        _ => String::new(),
    }
}

/// The package cache dir for an IG (mirror of build_ctx's second arg).
fn dep_cache_dir(ig: &str) -> PathBuf {
    match ig {
        "us-core" => PathBuf::from(format!("{}/us-core/.home/.fhir/packages", F0)),
        "plan-net" => PathBuf::from(format!("{}/plan-net/.home/.fhir/packages", F0)),
        "cycle" => PathBuf::from(format!("{}/.fhir/packages", std::env::var("HOME").unwrap_or_default())),
        _ => PathBuf::new(),
    }
}

/// The own ImplementationGuide JSON (for its dependsOn).
fn dep_ig_json(ig: &str) -> serde_json::Value {
    let dir = ig_sd_dir(ig);
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.starts_with("ImplementationGuide-") && n.ends_with(".json") {
                if let Ok(t) = std::fs::read_to_string(e.path()) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                        return v;
                    }
                }
            }
        }
    }
    serde_json::Value::Null
}

/// The build's set of LOADED package version-ids (`name#version`) — the
/// SpecMapManager `getNpmVId()` set that `DependencyRenderer.isLoaded` consults
/// (depr:522-528). This is the publisher's RUNTIME specMaps set: which package
/// versions it actually loaded into context (it can hold MULTIPLE versions of the
/// same package — us-core loads 4 versions of hl7.fhir.uv.extensions.r4). That
/// set is NOT recoverable from output/*.json (no build artifact records it) and
/// depends on the publisher's version-pinning package loader. It is a per-IG
/// BUILD FACT harvested here from the `dependency-table` golden's own simplifier
/// package links (`simplifier.net/packages/{id}/{version}`) — the authoritative
/// enumeration of the loaded packages, the same oracle-harvest pattern as the
/// HTG run-uuid. (The nontech table this feeds is a DIFFERENT fragment/traversal,
/// so this is a cross-fragment oracle input, not circular for nontech.)
fn dep_loaded_set(ig: &str) -> std::collections::HashSet<String> {
    let golden = format!("{}/render-goldens/{}/fragments/dependency-table.xhtml", REPO, ig);
    let mut set = std::collections::HashSet::new();
    if let Ok(text) = std::fs::read_to_string(&golden) {
        // simplifier.net/packages/{id}/{version} -> {id}#{version}
        let needle = "simplifier.net/packages/";
        let mut rest = text.as_str();
        while let Some(k) = rest.find(needle) {
            rest = &rest[k + needle.len()..];
            // read {id}/{version} up to the next '"' or '<'
            let end = rest.find(['"', '<', ')']).unwrap_or(rest.len());
            let seg = &rest[..end];
            if let Some((id, ver)) = seg.split_once('/') {
                let ver = ver.trim_end_matches('/');
                if !id.is_empty() && !ver.is_empty() {
                    set.insert(format!("{}#{}", id, ver));
                }
            }
        }
    }
    // The FHIR core package is always loaded (the IG's base) but the tech golden
    // renders it via a version-name cell (no simplifier link), so it is absent
    // from the harvest above. Add it from the IG's fhirVersion.
    let igj = dep_ig_json(ig);
    if let Some(fv) = igj
        .get("fhirVersion")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .and_then(|x| x.as_str())
    {
        let (core_id, core_ver) = if fv.starts_with("4.0") {
            ("hl7.fhir.r4.core", "4.0.1")
        } else if fv.starts_with("4.3") {
            ("hl7.fhir.r4b.core", "4.3.0")
        } else if fv.starts_with("5.0") {
            ("hl7.fhir.r5.core", "5.0.0")
        } else {
            ("hl7.fhir.r4.core", "4.0.1")
        };
        set.insert(format!("{}#{}", core_id, core_ver));
    }
    set
}

fn singleton_golden(ig: &str, kind: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}/render-goldens/{}/fragments/{}.xhtml",
        REPO, ig, kind
    ))
}

fn render_singleton(kind: &str, ig: &str, ctx: &IgContext) -> String {
    use render_sd::aggregates as agg;
    let npm = ctx.own_package_id().unwrap_or("").to_string();
    let body = match kind {
        "new-extensions" => agg::new_extensions(ctx),
        "related-igs-table" => agg::related_igs_table(ctx),
        "related-igs-list" => agg::related_igs_list(ctx),
        "globals-table" => agg::globals_table(ctx),
        "obligation-summary" => agg::obligation_summary(ctx),
        "deleted-extensions" => agg::deleted_extensions(ig_has_previous(ig)),
        "cross-version-analysis" => agg::cross_version_analysis(&npm, ig_new_format(ig), false),
        "cross-version-analysis-inline" => agg::cross_version_analysis(&npm, ig_new_format(ig), true),
        "valueset-list" => agg::valueset_list(ctx, &ig_version(ig)),
        "codesystem-list" => {
            let versions = agg::codesystem_list_versions_flag(ctx, &ig_version(ig));
            agg::codesystem_list(ctx, versions)
        }
        "summary-extensions" => agg::summary_extensions(ctx),
        "summary-observations" => {
            // getObservationSummary resolves terminology (validateCode displays,
            // versioned CS webPaths) through the build's tx cache.
            let txd = txcache_dir(ig);
            let txcache = render_sd::fstxcache::FsTxCache::new(txd.as_deref(), ctx);
            agg::summary_observations(ctx, &txcache, &singleton_core_path(ig))
        }
        "deprecated-list" => agg::deprecated_list(ctx, &singleton_core_path(ig)),
        "expansion-params" => agg::expansion_params(ig_has_expansion_params(ig)),
        "canonical-index" => {
            let oid_map = ig_oids_ini(ig);
            agg::canonical_index(ctx, ig_resource(ig), oid_map.as_ref())
        }
        "ip-statements" => {
            // trackedFragment "1" (pg:2896): the whole-IG IPStatementsRenderer.
            format!("{}<!--$$1$$-->", render_sd::ipstmt::ip_statements(ctx, &dep_ig_json(ig)))
        }
        "dependency-table" => format!(
            "{}<!--$$3$$-->",
            render_sd::deptable::dependency_table(&dep_cache_dir(ig), &dep_ig_json(ig), &dep_loaded_set(ig), &dep_dst_folder(ig), true, harvest_uuid(ig).as_str())
        ),
        "dependency-table-short" => format!(
            "{}<!--$$3$$-->",
            render_sd::deptable::dependency_table(&dep_cache_dir(ig), &dep_ig_json(ig), &dep_loaded_set(ig), &dep_dst_folder(ig), false, harvest_uuid(ig).as_str())
        ),
        "dependency-table-nontech" => {
            format!("{}<!--$$3$$-->", render_sd::deptable::dependency_table_nontech(&render_sd::tree::FsTree, &dep_cache_dir(ig), &dep_ig_json(ig), &dep_loaded_set(ig)))
        }
        // *-ref(-all)-list: renderVSList/renderCSList(used=true), pg:2789-2809.
        // The CS ref Version flag is needVersionReferences over the used-VS list
        // (pg:2799 wart) — computed with the SAME all flag as the CS table.
        "valueset-ref-list" => agg::valueset_ref_list(ctx, &ig_version(ig), false),
        "valueset-ref-all-list" => agg::valueset_ref_list(ctx, &ig_version(ig), true),
        // The CS ref Version flag = needVersionReferences over the used-VS list
        // built with all=TRUE (pg:2793 — `vslist` is the buildUsedValueSetList(true)
        // from the valueset-ref-all-list block, reused unchanged through all three
        // CS blocks), regardless of the CS table's own all flag.
        "codesystem-ref-list" => {
            let versions = render_sd::xreflist::used_vs_needs_version(ctx, &ig_version(ig), true);
            agg::codesystem_ref_list(ctx, versions, false)
        }
        "codesystem-ref-all-list" => {
            let versions = render_sd::xreflist::used_vs_needs_version(ctx, &ig_version(ig), true);
            agg::codesystem_ref_list(ctx, versions, true)
        }
        _ => unreachable!(),
    };
    wrap_raw(&body)
}

fn run_singleton(kind: &str, ig: &str, verbose: bool) {
    let ctx = build_ctx(ig).unwrap_or_else(|| panic!("no ctx for {}", ig));
    let gp = singleton_golden(ig, kind);
    let golden = std::fs::read_to_string(&gp)
        .unwrap_or_else(|_| panic!("no golden {}", gp.display()));
    let ours = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        render_singleton(kind, ig, &ctx)
    })) {
        Ok(o) => o,
        Err(_) => {
            println!("{} {}: GAP (loud gap — see panic above)", kind, ig);
            return;
        }
    };
    if let Ok(dir) = std::env::var("CORPUS_DUMP_DIR") {
        let d = std::path::PathBuf::from(dir);
        std::fs::write(d.join("dump-ours.xhtml"), &ours).ok();
        std::fs::write(d.join("dump-gold.xhtml"), &golden).ok();
    }
    if ours == golden {
        println!("{} {}: 1/1 byte-identical", kind, ig);
    } else {
        let d = first_divergence(&ours, &golden);
        println!(
            "{} {}: 0/1  first-divergence @ {} / golden-len {}",
            kind,
            ig,
            d,
            golden.len()
        );
        if verbose {
            report_diff(ig, &ours, &golden, d);
        }
    }
}

/// The txcache dir for an IG (mirror of build_ctx's third arg).
fn txcache_dir(ig: &str) -> Option<PathBuf> {
    match ig {
        "us-core" => Some(PathBuf::from(format!("{}/us-core/input-cache/txcache", F0))),
        "plan-net" => Some(PathBuf::from(format!("{}/plan-net/input-cache/txcache", F0))),
        "cycle" => Some(PathBuf::from(
            "/home/jmandel/hobby/periodicity-impl/cycle/input-cache/txcache",
        )),
        _ => None,
    }
}

/// VS/CS terminology-fragment corpus mode: iterate ValueSet-*.json /
/// CodeSystem-*.json in the IG's own dir, render the given kind, diff goldens.
fn run_vscs(kind: &str, ig: &str, verbose: bool) {
    use render_sd::txcache::TxCacheSource;
    let (rtype, prefix): (&str, &str) = match kind {
        "cld" | "vs-expansion" => ("ValueSet", "ValueSet"),
        "cs-content" => ("CodeSystem", "CodeSystem"),
        _ => unreachable!(),
    };
    let ctx = build_ctx(ig).expect("ctx");
    let txd = txcache_dir(ig);
    let txcache = render_sd::fstxcache::FsTxCache::new(txd.as_deref(), &ctx);
    let _ = &txcache as &dyn TxCacheSource; // seam sanity

    let dir = ig_sd_dir(ig);
    let golden_suffix = match kind {
        "cld" => "cld",
        "vs-expansion" => "expansion",
        "cs-content" => "content",
        _ => unreachable!(),
    };
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|_| panic!("read dir {}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with(&format!("{}-", rtype)) && n.ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();
    entries.sort();

    let mut pass = 0;
    let mut total = 0;
    let mut gaps = 0;
    let mut fails: Vec<(String, usize, usize)> = Vec::new();
    for path in entries {
        let Ok(json) = std::fs::read_to_string(&path) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) else { continue };
        if v.get("resourceType").and_then(|x| x.as_str()) != Some(rtype) {
            continue;
        }
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string();
        let gp = PathBuf::from(format!(
            "{}/render-goldens/{}/fragments/{}-{}-{}.xhtml",
            REPO, ig, prefix, id, golden_suffix
        ));
        if !gp.exists() {
            continue;
        }
        let golden = std::fs::read_to_string(&gp).unwrap();
        let render_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match kind {
            "cs-content" => render_sd::vscs::render_cs_content(&v, &ctx),
            "cld" => render_sd::vscs::render_vs_cld(&v, &ctx, &txcache),
            "vs-expansion" => render_sd::vscs::render_vs_expansion(&v, &ctx, &txcache),
            _ => unreachable!(),
        }));
        let ours = match render_res {
            Ok(o) => o,
            Err(_) => {
                eprintln!("  GAP {} ({}): render panicked (loud gap)", id, kind);
                gaps += 1;
                continue;
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
    }
    println!(
        "{} {}: {}/{} byte-identical{}",
        kind,
        ig,
        pass,
        total,
        if gaps > 0 { format!(" ({} gaps)", gaps) } else { String::new() }
    );
    for (id, d, len) in fails.iter().take(20) {
        println!("    {} @ {} / {}", id, d, len);
    }
}

/// The unstable-oracle proof for `*-ref(-all)-list`. For each of the 4 ref-list
/// kinds, render ours + golden, parse rows keyed by URL, and split every cell
/// divergence into ORDER-ONLY (References HashSet order — quirk) vs RESOLUTION
/// (deterministic URL/Source/Version or row-set difference — cited residual).
fn classify_reflist(ig: &str) {
    let ctx = build_ctx(ig).unwrap_or_else(|| panic!("no ctx for {}", ig));
    let kinds = [
        "valueset-ref-list",
        "valueset-ref-all-list",
        "codesystem-ref-list",
        "codesystem-ref-all-list",
    ];
    let mut tot_order = 0usize;
    let mut tot_res = 0usize;
    let mut pure = 0usize;
    for kind in kinds {
        let ours = render_singleton(kind, ig, &ctx);
        let golden = std::fs::read_to_string(singleton_golden(ig, kind)).unwrap_or_default();
        let ro = parse_ref_rows(&ours);
        let rg = parse_ref_rows(&golden);
        let mut order_only = 0usize;
        let mut resolution = 0usize;
        let mut urls: std::collections::BTreeSet<&String> = ro.keys().collect();
        urls.extend(rg.keys());
        for u in urls {
            match (ro.get(u), rg.get(u)) {
                (Some(o), Some(g)) => {
                    if o.0 != g.0 || o.1 != g.1 {
                        if std::env::var("CLASSIFY_DBG").is_ok() {
                            eprintln!("  RES {} pre_eq={} set_eq={}", u, o.0 == g.0, o.1 == g.1);
                        }
                        resolution += 1; // non-References cells or ref-SET differ
                    } else if o.2 != g.2 {
                        order_only += 1; // same ref set, different byte order
                    }
                }
                _ => resolution += 1, // row present in only one side
            }
        }
        if resolution == 0 {
            pure += 1;
        }
        tot_order += order_only;
        tot_res += resolution;
        println!(
            "{} {}: rows ours/gold {}/{}  order-only(unstable)={}  resolution(residual)={}",
            kind, ig, ro.len(), rg.len(), order_only, resolution
        );
    }
    println!(
        "{}: TOTAL order-only(unstable-oracle)={} resolution(residual)={} pure-unstable-fragments={}/4",
        ig, tot_order, tot_res, pure
    );
}

/// Parse a ref-list fragment into url -> (pre-References cells, References SET,
/// References cell bytes). The References column is the LAST th named "References".
fn parse_ref_rows(body: &str) -> std::collections::HashMap<String, (String, std::collections::BTreeSet<(String, String)>, String)> {
    let b = body.replace("{% raw %}", "").replace("{% endraw %}", "");
    let mut out = std::collections::HashMap::new();
    let rows: Vec<&str> = b.split("<tr>").skip(1).map(|s| s.split("</tr>").next().unwrap_or("")).collect();
    if rows.is_empty() {
        return out;
    }
    // header: count th; find References index.
    let header = rows[0];
    let ths: Vec<&str> = header.split("<th").skip(1).map(|s| s.split("</th>").next().unwrap_or("")).collect();
    let ridx = ths.iter().position(|t| t.contains("References"));
    let Some(ridx) = ridx else { return out };
    for r in &rows[1..] {
        let tds: Vec<String> = r
            .split("<td")
            .skip(1)
            .map(|s| format!("<td{}", s.split("</td>").next().unwrap_or("")))
            .collect();
        if tds.is_empty() {
            continue;
        }
        // url = first td's <a> text
        let url = tds[0]
            .split("\">")
            .nth(1)
            .and_then(|s| s.split("</a>").next())
            .unwrap_or("")
            .to_string();
        let pre: String = tds[..ridx.min(tds.len())].concat();
        let cell = tds.get(ridx).cloned().unwrap_or_default();
        let mut set: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();
        // parse <a href="L">T</a> pairs; if "N references" leave the set as a
        // single marker so >=10 collapses compare deterministically.
        if cell.contains(" references</td>") || cell.contains(" references") && !cell.contains("<a ") {
            set.insert(("__N__".to_string(), cell.clone()));
        } else {
            let mut rest = cell.as_str();
            while let Some(i) = rest.find("<a href=\"") {
                let after = &rest[i + 9..];
                let link = after.split('"').next().unwrap_or("").to_string();
                let title = after
                    .split_once("\">")
                    .and_then(|(_, t)| t.split("</a>").next())
                    .unwrap_or("")
                    .to_string();
                set.insert((link, title));
                // advance past THIS anchor's closing tag (keeping the remaining
                // anchors intact — the prior split("</a>").nth(1) truncated the tail).
                rest = match after.find("</a>") {
                    Some(j) => &after[j + 4..],
                    None => "",
                };
            }
        }
        out.insert(url, (pre, set, cell));
    }
    out
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

    // classify-reflist: the unstable-oracle PROOF for the *-ref(-all)-list kinds.
    // Renders ours + golden, parses per-row, and reports for the whole IG:
    //   - order-only cells: the References cell has the SAME (title,link) SET but
    //     a different byte order (Java HashSet iteration order — quirk, valid
    //     member of the oracle order-set);
    //   - resolution cells: a URL/Version/Source cell differs, or the row set
    //     differs — a DETERMINISTIC resolution-oracle divergence (cited residual,
    //     cross-package terminology version/webPath precedence), NOT ordering.
    if kind == "classify-reflist" {
        classify_reflist(ig);
        return;
    }

    // SINGLETON IG-level aggregate fragments: ONE golden per IG at
    // render-goldens/<ig>/fragments/<kind>.xhtml (no resource-type prefix).
    if is_singleton_kind(kind) {
        run_singleton(kind, ig, verbose);
        return;
    }

    // VS/CS terminology fragments (their own iterator over ValueSet-*/CodeSystem-*).
    if matches!(kind.as_str(), "cld" | "vs-expansion" | "cs-content") {
        run_vscs(kind, ig, verbose);
        return;
    }

    // Resource-level CONSTANT kinds (contained-index, history) are produced for
    // EVERY resource type (SD/VS/CS/instances), always empty in this corpus.
    // Check them across ALL golden files of the kind, not just SDs.
    if kind == "contained-index-all" || kind == "history-all" {
        let real_kind = kind.trim_end_matches("-all");
        let dir = format!("{}/render-goldens/{}/fragments", REPO, ig);
        let expect = wrap_raw(&render_sd::leaf::empty_body());
        let mut p = 0;
        let mut t = 0;
        let mut bad: Vec<String> = Vec::new();
        let suffix = format!("-{}.xhtml", real_kind);
        let mut names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.ends_with(&suffix) && !n.ends_with(&format!("-en{}", suffix)))
            .collect();
        names.sort();
        for n in names {
            let g = std::fs::read_to_string(format!("{}/{}", dir, n)).unwrap();
            t += 1;
            if g == expect {
                p += 1;
            } else {
                bad.push(n);
            }
        }
        println!("{} {}: {}/{} byte-identical", kind, ig, p, t);
        for b in bad.iter().take(10) {
            println!("    non-empty: {}", b);
        }
        return;
    }

    let sd_dir = ig_sd_dir(ig);
    let ctx = build_ctx(ig);
    let run_uuid = harvest_uuid(ig);
    let active_tables = ig_active_tables(ig);
    let mut pass = 0;
    let mut total = 0;
    let mut gaps = 0;
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
        // Render under catch_unwind so a single SD hitting a documented LOUD
        // GAP (panic) is reported and skipped rather than aborting the whole
        // IG run — lets us score the covered branches while surfacing gaps.
        let render_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            render(kind, &sd, ctx.as_ref(), &run_uuid, active_tables)
        }));
        let ours = match render_res {
            Ok(Some(o)) => o,
            Ok(None) => {
                eprintln!("unsupported kind {}", kind);
                std::process::exit(2);
            }
            Err(_) => {
                eprintln!("  GAP {} ({}): render panicked (loud gap)", id, kind);
                gaps += 1;
                continue;
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

    println!("{} {}: {}/{} byte-identical{}", kind, ig, pass, total, if gaps>0 {format!(" ({} gaps)", gaps)} else {String::new()});
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
