//! Byte-parity regression pins for the C1 snapshot table path.
//! Inputs: publisher post-snapshot SDs (F0 builds / cycle temp/pages).
//! Skips (rather than fails) when local build inputs are absent.

use std::path::{Path, PathBuf};

use render_sd::context::IgContext;
use render_sd::table::{render_table, TableConfig};
use render_sd::{wrap_raw, Sd};

const REPO: &str = "/home/jmandel/hobby/sushi-rs-snapshot";
const F0: &str = "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds";

fn harvest_uuid(ig: &str) -> String {
    let dir = format!("{}/render-goldens/{}/fragments", REPO, ig);
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.ends_with("-snapshot.xhtml") {
            if let Ok(text) = std::fs::read_to_string(e.path()) {
                if let Some(i) = text.find("  // ") {
                    let rest = &text[i + 5..];
                    if let Some(j) = rest.find('\n') {
                        if j == 36 {
                            return rest[..j].to_string();
                        }
                    }
                }
            }
        }
    }
    String::new()
}

fn check(ig: &str, id: &str, active_tables: bool) {
    check_kind(ig, id, active_tables, "snapshot", |u| TableConfig::snapshot(u));
}

fn check_kind(
    ig: &str,
    id: &str,
    active_tables: bool,
    suffix: &str,
    mk: impl Fn(&str) -> TableConfig,
) {
    let (own, pkgs, txc): (String, String, Option<String>) = match ig {
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
        _ => panic!("unknown ig"),
    };
    let sd_path = PathBuf::from(format!("{}/StructureDefinition-{}.json", own, id));
    if !sd_path.exists() {
        eprintln!("skip {}/{}: input absent", ig, id);
        return;
    }
    let ctx = IgContext::load_with_txcache(
        Path::new(&own),
        Path::new(&pkgs),
        txc.as_deref().map(Path::new),
    );
    let sd = Sd::from_json(&std::fs::read_to_string(&sd_path).unwrap()).unwrap();
    let def_file = format!("StructureDefinition-{}-definitions.html", sd.id());
    let mut cfg = mk(&harvest_uuid(ig));
    cfg.active_tables = active_tables;
    let (body, _gaps) = render_table(&sd, &ctx, &def_file, &cfg);
    let ours = wrap_raw(&body);
    let golden = std::fs::read_to_string(format!(
        "{}/render-goldens/{}/fragments/StructureDefinition-{}-{}.xhtml",
        REPO, ig, id, suffix
    ))
    .unwrap();
    assert_eq!(ours, golden, "{} parity failed for {}/{}", suffix, ig, id);
}

#[test]
fn snapshot_us_core_patient() {
    check("us-core", "us-core-patient", false);
}

#[test]
fn snapshot_us_core_medicationrequest() {
    check("us-core", "us-core-medicationrequest", false);
}

#[test]
fn snapshot_us_core_authentication_time() {
    check("us-core", "us-core-authentication-time", false);
}

#[test]
fn snapshot_plan_net_healthcareservice() {
    check("plan-net", "plannet-HealthcareService", true);
}

#[test]
fn snapshot_plan_net_organization() {
    check("plan-net", "plannet-Organization", true);
}

#[test]
fn by_mustsupport_us_core_patient() {
    check_kind(
        "us-core",
        "us-core-patient",
        false,
        "snapshot-by-mustsupport",
        TableConfig::snapshot_by_mustsupport,
    );
}

#[test]
fn by_mustsupport_us_core_medicationrequest() {
    check_kind(
        "us-core",
        "us-core-medicationrequest",
        false,
        "snapshot-by-mustsupport",
        TableConfig::snapshot_by_mustsupport,
    );
}

#[test]
fn by_mustsupport_plan_net_organization() {
    check_kind(
        "plan-net",
        "plannet-Organization",
        true,
        "snapshot-by-mustsupport",
        TableConfig::snapshot_by_mustsupport,
    );
}

#[test]
fn by_key_us_core_patient() {
    check_kind(
        "us-core",
        "us-core-patient",
        false,
        "snapshot-by-key",
        TableConfig::snapshot_by_key,
    );
}

#[test]
fn by_key_us_core_head_circumference() {
    check_kind(
        "us-core",
        "head-occipital-frontal-circumference-percentile",
        false,
        "snapshot-by-key",
        TableConfig::snapshot_by_key,
    );
}

#[test]
fn by_key_plan_net_organization() {
    check_kind(
        "plan-net",
        "plannet-Organization",
        true,
        "snapshot-by-key",
        TableConfig::snapshot_by_key,
    );
}
