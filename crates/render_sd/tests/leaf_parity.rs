//! Byte-parity pins for F4 SD leaf fragment kinds. Skips when F0 inputs absent.

use std::path::{Path, PathBuf};

use render_sd::context::IgContext;
use render_sd::leaf::{self, GenMode};
use render_sd::{wrap_raw, Sd};

const REPO: &str = "/home/jmandel/hobby/sushi-rs-snapshot";
const F0: &str = "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds";

fn ctx_us_core() -> Option<IgContext> {
    let own = format!("{}/us-core/output", F0);
    if !Path::new(&own).exists() {
        return None;
    }
    Some(IgContext::load_with_txcache(
        Path::new(&own),
        Path::new(&format!("{}/us-core/.home/.fhir/packages", F0)),
        Some(Path::new(&format!("{}/us-core/input-cache/txcache", F0))),
    ))
}

fn golden(ig: &str, id: &str, kind: &str) -> Option<String> {
    let p = PathBuf::from(format!(
        "{}/render-goldens/{}/fragments/StructureDefinition-{}-{}.xhtml",
        REPO, ig, id, kind
    ));
    std::fs::read_to_string(p).ok()
}

fn load_sd(ig: &str, id: &str) -> Option<Sd> {
    let p = format!("{}/{}/output/StructureDefinition-{}.json", F0, ig, id);
    let j = std::fs::read_to_string(p).ok()?;
    Sd::from_json(&j).ok()
}

#[test]
fn constant_leaves_us_core() {
    let id = "head-occipital-frontal-circumference-percentile";
    if golden("us-core", id, "contained-index").is_none() {
        eprintln!("skip: goldens absent");
        return;
    }
    assert_eq!(
        wrap_raw(&leaf::empty_body()),
        golden("us-core", id, "contained-index").unwrap()
    );
    assert_eq!(
        wrap_raw(&leaf::empty_body()),
        golden("us-core", id, "history").unwrap()
    );
    assert_eq!(
        wrap_raw(&leaf::pseudo_ttl()),
        golden("us-core", id, "pseudo-ttl").unwrap()
    );
    assert_eq!(
        wrap_raw(&leaf::pseudo_xml()),
        golden("us-core", id, "pseudo-xml").unwrap()
    );
}

#[test]
fn inv_us_core() {
    let Some(ctx) = ctx_us_core() else {
        eprintln!("skip: F0 absent");
        return;
    };
    let id = "head-occipital-frontal-circumference-percentile";
    let Some(sd) = load_sd("us-core", id) else {
        eprintln!("skip");
        return;
    };
    let ours = wrap_raw(&leaf::inv(&sd, &ctx, true, GenMode::Snap, true));
    assert_eq!(ours, golden("us-core", id, "inv").unwrap());
    let ours_k = wrap_raw(&leaf::inv(&sd, &ctx, true, GenMode::Key, true));
    assert_eq!(ours_k, golden("us-core", id, "inv-key").unwrap());
}
