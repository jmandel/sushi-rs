//! Byte-parity regression tests for the C1 grid path against the committed
//! golden corpus. These pin the fragment kinds already at byte parity so a
//! regression is caught immediately.
//!
//! Inputs are the publisher's snapshot-complete SDs from the F0 build
//! (`../sushi-rs-snapshot-f0-builds/<ig>/output/`). If that build is absent the
//! test is skipped (returns early) rather than failing — the goldens are always
//! present but the F0 inputs are a local artifact.

use std::path::{Path, PathBuf};

use render_sd::context::IgContext;
use render_sd::grid::render_grid;
use render_sd::{wrap_raw, Sd};

const REPO: &str = "/home/jmandel/hobby/sushi-rs-snapshot";
const F0: &str = "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds";

fn build_ctx(ig: &str) -> IgContext {
    IgContext::load_with_txcache(
        Path::new(&format!("{}/{}/output", F0, ig)),
        Path::new(&format!("{}/{}/.home/.fhir/packages", F0, ig)),
        Some(Path::new(&format!("{}/{}/input-cache/txcache", F0, ig))),
    )
}

fn check_grid(ig: &str, id: &str) {
    let sd_path = PathBuf::from(format!(
        "{}/{}/output/StructureDefinition-{}.json",
        F0, ig, id
    ));
    if !sd_path.exists() {
        eprintln!("skip {}/{}: F0 input absent", ig, id);
        return;
    }
    let golden_path = PathBuf::from(format!(
        "{}/render-goldens/{}/fragments/StructureDefinition-{}-grid.xhtml",
        REPO, ig, id
    ));
    let json = std::fs::read_to_string(&sd_path).unwrap();
    let sd = Sd::from_json(&json).unwrap();
    let ctx = build_ctx(ig);
    let def_file = format!("StructureDefinition-{}-definitions.html", sd.id());
    let ours = wrap_raw(&render_grid(&sd, &ctx, &def_file, ""));
    let golden = std::fs::read_to_string(&golden_path).unwrap();
    assert_eq!(ours, golden, "grid parity failed for {}/{}", ig, id);
}

#[test]
fn grid_us_core_authentication_time() {
    check_grid("us-core", "us-core-authentication-time");
}

#[test]
fn grid_us_core_jurisdiction() {
    check_grid("us-core", "us-core-jurisdiction");
}

#[test]
fn grid_us_core_birthsex() {
    check_grid("us-core", "us-core-birthsex");
}

#[test]
fn grid_plan_net_accessibility() {
    check_grid("plan-net", "accessibility");
}
