//! Byte-parity gate for [`package_store::template_loader::materialize`] against the
//! Java-materialized F0 `template/` trees — the free oracle (we hold both sides).
//!
//! Two chains, both fetched deterministically from the F0 builds' pinned template
//! caches:
//!   - **us-core** (parity): `hl7.fhir.template#1.0.0` → `hl7.base.template#1.0.0`
//!     → `fhir.base.template#1.0.0` (3 packages).
//!   - **plan-net** (generalization): `hl7.davinci.template#current` →
//!     `hl7.fhir.template#current` → `hl7.base.template#current` →
//!     `fhir2.base.template#current` (4 packages, `fhir2.base` root, davinci leaf,
//!     `translations/`, `multilanguage-format`).
//!
//! Every staged file is accounted for: **byte-identical to our materialization**,
//! or classified as an **ant runtime product** (a build product the site never
//! reads — [`is_ant_runtime_product`]), or an **ant-overwritten source** (a real
//! package file the ant translation step rewrites in place — our static
//! materialization stages the RAW package bytes, verified against the source
//! package). No file is hand-waved.
//!
//! Inputs are a LOCAL artifact (the F0 builds, ~18MB of template binaries per
//! chain — not vendored). If that build is absent the test skips (returns early)
//! rather than failing, exactly like the render_sd parity gates. The tiny in-repo
//! `template_loader::tests` unit tests always run and pin the merge rules on
//! synthetic chains; this gate pins them byte-exact on the real packages when the
//! F0 build is present.

use package_store::template_loader::{
    is_ant_overwritten_source, is_ant_runtime_product, materialize, TemplatePaths,
};
use package_store::DiskSource;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const F0: &str = "/home/jmandel/hobby/sushi-rs-snapshot-f0-builds";

fn walk(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                out.insert(rel, std::fs::read(&p).unwrap());
            }
        }
    }
    out
}

/// Materialize `root_label` from the F0 chain cache and account for every staged
/// file. `overwrite_src_pkg` is the label of the package that holds the RAW form
/// of any ant-overwritten source (the translation root); pass `None` if the chain
/// has none.
fn gate(build: &str, root_label: &str, overwrite_src_pkg: Option<&str>) {
    let base = PathBuf::from(F0).join(build);
    let staged = base.join("template");
    if !staged.is_dir() {
        eprintln!("skip {build}: F0 template tree absent at {}", staged.display());
        return;
    }
    let cache = base.join(".home/.fhir/packages");

    let paths = TemplatePaths::new(&cache);
    let tree = materialize(&DiskSource, &paths, root_label).expect("materialize");
    let f0_files = walk(&staged);

    let mut identical = 0usize;
    let mut runtime_excluded = 0usize;
    let mut overwritten_ok = 0usize;
    let mut differ: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut extra: Vec<String> = Vec::new();

    for (rel, f0bytes) in &f0_files {
        let base_name = rel.rsplit('/').next().unwrap();
        if base_name == ".index.json" || base_name == ".index.db" {
            continue; // package-cache indexing sidecar, not content.
        }
        if is_ant_runtime_product(rel) {
            runtime_excluded += 1;
            continue;
        }
        if is_ant_overwritten_source(rel) {
            // Staged bytes are the ant-regenerated form; verify OUR bytes equal the
            // RAW package source (correct static materialization).
            let ours = tree.get(rel).expect("overwritten source materialized");
            let src_pkg = overwrite_src_pkg.expect("chain has an overwritten source pkg");
            let raw = std::fs::read(cache.join(src_pkg).join(rel)).expect("raw source file");
            assert_eq!(ours, raw.as_slice(), "ant-overwritten {rel}: ours must equal RAW pkg bytes");
            overwritten_ok += 1;
            continue;
        }
        match tree.get(rel) {
            Some(ours) if ours == f0bytes.as_slice() => identical += 1,
            Some(_) => differ.push(rel.clone()),
            None => missing.push(rel.clone()),
        }
    }
    for rel in tree.files().keys() {
        if !f0_files.contains_key(rel) {
            extra.push(rel.clone());
        }
    }

    eprintln!(
        "[{build}] staged={} identical={identical} ant-runtime={runtime_excluded} \
         ant-overwritten={overwritten_ok} differ={} missing={} extra={}",
        f0_files.len(),
        differ.len(),
        missing.len(),
        extra.len()
    );
    assert!(differ.is_empty(), "{build}: byte differences: {differ:?}");
    assert!(missing.is_empty(), "{build}: files missing from our tree: {missing:?}");
    assert!(extra.is_empty(), "{build}: extra files in our tree: {extra:?}");
    assert!(identical > 100, "{build}: sanity — expected many identical files");
}

/// Parity gate: us-core 3-package chain, byte-exact.
#[test]
fn us_core_template_byte_parity() {
    gate("us-core", "hl7.fhir.template#1.0.0", None);
}

/// Generalization gate: plan-net 4-package davinci chain, byte-exact. The
/// `translations/strings*.json` are ant-overwritten sources whose RAW form lives
/// in `fhir2.base.template#current`.
#[test]
fn plan_net_template_byte_parity() {
    gate(
        "plan-net",
        "hl7.davinci.template#current",
        Some("fhir2.base.template#current"),
    );
}
